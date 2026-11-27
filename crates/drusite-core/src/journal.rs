// SPDX-License-Identifier: AGPL-3.0-or-later

//! The journal, page queries and the settings that sync with the account.

use drusite_crdt::SettingValue;
use drusite_model::journal::ASK_JOURNAL_SETTING;
use drusite_model::{Day, PageId, PageKind, PageQuery, Property, PropertyValue, journal};

use crate::api::{ApiError, ErrorCode, PageInfo, Setting, Settings};
use crate::engine::Core;

const JOURNAL_PARENT: &str = "journal.parent";
const JOURNAL_TEMPLATE: &str = "journal.template";
const WEEK_START: &str = "weekStart";

/// The title of the page journal entries go under when nobody has picked one.
pub const JOURNAL_TITLE: &str = "Journal";

impl Core {
    pub(crate) fn settings(&self) -> Result<Settings, ApiError> {
        let mut settings = Settings::default();
        // A page in the trash no longer counts.
        let exists = |id: PageId| self.workspace.is_trashed(id).is_ok_and(|t| !t);
        for (key, value) in self.workspace.settings()? {
            match (key.as_str(), value) {
                (JOURNAL_PARENT, SettingValue::Text(id)) => {
                    settings.journal_parent = id.parse().ok().filter(|&id| exists(id));
                }
                (JOURNAL_TEMPLATE, SettingValue::Text(id)) => {
                    settings.journal_template = id.parse().ok().filter(|&id| exists(id));
                }
                (WEEK_START, SettingValue::Int(day)) => {
                    settings.week_start = u8::try_from(day.clamp(0, 6)).unwrap_or(1);
                }
                (ASK_JOURNAL_SETTING, SettingValue::Bool(on)) => settings.ask_journal = on,
                _ => {}
            }
        }
        Ok(settings)
    }

    pub(crate) fn set_setting(&mut self, setting: Setting) -> Result<(), ApiError> {
        let page = |id: Option<PageId>| -> Result<Option<SettingValue>, ApiError> {
            match id {
                Some(id) => {
                    self.workspace.title(id)?;
                    Ok(Some(SettingValue::Text(id.to_string())))
                }
                None => Ok(None),
            }
        };
        let (key, value) = match setting {
            Setting::JournalParent(id) => (JOURNAL_PARENT, page(id)?),
            Setting::JournalTemplate(id) => (JOURNAL_TEMPLATE, page(id)?),
            Setting::WeekStart(day) if day <= 6 => {
                (WEEK_START, Some(SettingValue::Int(i64::from(day))))
            }
            // Off is stored as absent: the default, and what an older device knows.
            Setting::AskJournal(on) => {
                (ASK_JOURNAL_SETTING, on.then_some(SettingValue::Bool(true)))
            }
            Setting::WeekStart(day) => {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    format!("week start {day} isn't a weekday (0-6)"),
                ));
            }
        };
        self.workspace.set_setting(key, value)?;
        self.save_workspace()
    }

    pub(crate) fn query_pages(&self, query: &PageQuery) -> Result<Vec<PageInfo>, ApiError> {
        let mut pages: Vec<PageInfo> = self
            .pages()?
            .into_iter()
            .filter(|p| query.kind.is_none_or(|k| k == p.kind))
            .filter(|p| {
                query
                    .conditions
                    .iter()
                    .all(|c| c.matches(property(&p.properties, &c.property)))
            })
            .collect();
        if let Some(sort) = &query.sort {
            pages.sort_by(|a, b| {
                let order = if sort.by == "title" {
                    a.title.to_lowercase().cmp(&b.title.to_lowercase())
                } else {
                    // Pages without the property go last, whichever way the sort runs.
                    match (
                        property(&a.properties, &sort.by),
                        property(&b.properties, &sort.by),
                    ) {
                        (Some(x), Some(y)) => {
                            let o = x.compare(y);
                            if sort.descending { o.reverse() } else { o }
                        }
                        (Some(_), None) => return std::cmp::Ordering::Less,
                        (None, Some(_)) => return std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    }
                };
                // Ties keep the order pages were made in.
                let order = if sort.by == "title" && sort.descending {
                    order.reverse()
                } else {
                    order
                };
                order.then(a.id.cmp(&b.id))
            });
        }
        if let Some(limit) = query.limit {
            pages.truncate(limit as usize);
        }
        Ok(pages)
    }

    /// The journal entry for `date`: the oldest one there is (two devices offline can each make
    /// one), or a new one made from the template.
    pub(crate) fn journal_day(&mut self, date: Day) -> Result<(PageInfo, bool), ApiError> {
        let wanted = PropertyValue::Date(date);
        let existing = self
            .pages()?
            .into_iter()
            .filter(|p| p.kind == PageKind::Journal)
            .filter(|p| property(&p.properties, journal::DATE) == Some(&wanted))
            .min_by_key(|p| p.id);
        if let Some(page) = existing {
            return Ok((page, false));
        }

        let settings = self.settings()?;
        let parent = match settings.journal_parent {
            Some(id) => id,
            None => self.make_journal_parent()?,
        };
        let template = match settings.journal_template {
            Some(id) => Some(self.page_blocks(id)?),
            None => None,
        };
        let mut page = self.workspace.create_page_of(
            Some(parent),
            &date.title(),
            PageKind::Journal,
            &[Property {
                name: journal::DATE.to_owned(),
                value: wanted,
            }],
        )?;
        if let Some(blocks) = template {
            page.fill_from(&blocks, |text| fill_in(text, date))?;
        }
        self.store.create_page(&page)?;
        self.save_workspace()?;
        let info = self
            .pages()?
            .into_iter()
            .find(|p| p.id == page.id())
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, "journal entry vanished"))?;
        Ok((info, true))
    }

    /// A top-level "Journal" page, remembered in the settings.
    fn make_journal_parent(&mut self) -> Result<PageId, ApiError> {
        let page = self.workspace.create_page(None, JOURNAL_TITLE)?;
        self.store.create_page(&page)?;
        let id = page.id();
        self.workspace
            .set_setting(JOURNAL_PARENT, Some(SettingValue::Text(id.to_string())))?;
        self.save_workspace()?;
        Ok(id)
    }

    /// A page's blocks, from its open copy if it's open.
    fn page_blocks(&mut self, id: PageId) -> Result<Vec<drusite_model::Block>, ApiError> {
        if let Some(open) = self.open.get(&id) {
            return Ok(open.page.blocks()?);
        }
        Ok(self.store.load_page(id)?.0.blocks()?)
    }
}

fn property<'a>(properties: &'a [Property], name: &str) -> Option<&'a PropertyValue> {
    properties.iter().find(|p| p.name == name).map(|p| &p.value)
}

/// Fills in a template's placeholders for `date`.
fn fill_in(text: &str, date: Day) -> String {
    if !text.contains("{{") {
        return text.to_owned();
    }
    text.replace("{{date}}", &date.to_string())
        .replace("{{title}}", &date.title())
        .replace("{{weekday}}", date.weekday())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::{Arc, Mutex};

    use drusite_model::{BlockKind, Comparison, Condition, HeadingLevel, Sort};

    use super::*;
    use crate::StoreKey;
    use crate::api::{Command, Edit, Event, Response};

    struct Fixture {
        core: Core,
        events: Arc<Mutex<Vec<Event>>>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let core = Core::open(
            dir.path(),
            &StoreKey::generate().unwrap(),
            Box::new(move |e| sink.lock().unwrap().push(e)),
        )
        .unwrap();
        Fixture {
            core,
            events,
            _dir: dir,
        }
    }

    fn day(s: &str) -> Day {
        s.parse().unwrap()
    }

    fn open_day(core: &mut Core, date: &str) -> (PageInfo, bool) {
        match core
            .handle(Command::OpenJournalDay { date: day(date) })
            .unwrap()
        {
            Response::JournalDay { page, created } => (page, created),
            other => panic!("{other:?}"),
        }
    }

    fn set_mood(core: &mut Core, page: PageId, mood: f64) {
        core.handle(Command::SetProperty {
            page,
            name: journal::MOOD.into(),
            value: Some(PropertyValue::Number(mood)),
        })
        .unwrap();
    }

    fn settings(core: &mut Core) -> Settings {
        match core.handle(Command::Settings).unwrap() {
            Response::Settings { settings } => settings,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_day_is_made_once_under_the_journal() {
        let mut f = fixture();
        let (entry, created) = open_day(&mut f.core, "2026-11-26");
        assert!(created);
        assert_eq!(entry.kind, PageKind::Journal);
        assert_eq!(entry.title, "Thursday, 26 November 2026");
        assert_eq!(
            entry.properties,
            [Property {
                name: "Date".into(),
                value: PropertyValue::Date(day("2026-11-26")),
            }]
        );
        let parent = settings(&mut f.core).journal_parent.unwrap();
        assert_eq!(entry.parent, Some(parent));
        assert_eq!(f.core.workspace.title(parent).unwrap(), JOURNAL_TITLE);

        let (again, created) = open_day(&mut f.core, "2026-11-26");
        assert!(!created);
        assert_eq!(again.id, entry.id);
        let (next, _) = open_day(&mut f.core, "2026-11-27");
        assert_eq!(next.parent, Some(parent));
        assert_ne!(next.id, entry.id);
    }

    #[test]
    fn a_day_is_made_from_the_template() {
        let mut f = fixture();
        let Response::PageCreated { page: template } = f
            .core
            .handle(Command::CreatePage {
                parent: None,
                title: "Daily template".into(),
            })
            .unwrap()
        else {
            panic!()
        };
        let Response::Opened { view } = f
            .core
            .handle(Command::OpenPage { page: template.id })
            .unwrap()
        else {
            panic!()
        };
        let first = view.blocks[0].id;
        f.core
            .handle(Command::Edit {
                page: template.id,
                edits: vec![
                    Edit::SetKind {
                        block: first,
                        kind: BlockKind::Heading {
                            level: HeadingLevel::H2,
                        },
                    },
                    Edit::InsertText {
                        block: first,
                        pos: 0,
                        text: "How was {{weekday}}?".into(),
                    },
                    Edit::Split {
                        block: first,
                        pos: 20,
                        kind: BlockKind::Todo { checked: false },
                    },
                ],
            })
            .unwrap();
        f.core
            .handle(Command::SetSetting {
                setting: Setting::JournalTemplate(Some(template.id)),
            })
            .unwrap();
        let (entry, _) = open_day(&mut f.core, "2026-11-26");
        let Response::Opened { view } =
            f.core.handle(Command::OpenPage { page: entry.id }).unwrap()
        else {
            panic!()
        };
        let texts: Vec<_> = view.blocks.iter().map(|b| b.text()).collect();
        assert_eq!(texts, ["How was Thursday?", ""]);
        assert_eq!(view.blocks[1].kind, BlockKind::Todo { checked: false });
    }

    #[test]
    fn queries_filter_sort_and_limit() {
        let mut f = fixture();
        for (date, mood) in [
            ("2026-11-20", 2.0),
            ("2026-11-21", 5.0),
            ("2026-11-22", 4.0),
            ("2026-11-23", 1.0),
        ] {
            let (page, _) = open_day(&mut f.core, date);
            set_mood(&mut f.core, page.id, mood);
        }
        open_day(&mut f.core, "2026-11-24"); // no mood yet
        let query = PageQuery {
            kind: Some(PageKind::Journal),
            conditions: vec![Condition {
                property: journal::MOOD.into(),
                comparison: Comparison::GreaterOrEqual,
                value: Some(PropertyValue::Number(4.0)),
            }],
            sort: Some(Sort {
                by: journal::DATE.into(),
                descending: true,
            }),
            limit: None,
        };
        let Response::Pages { pages } = f.core.handle(Command::QueryPages { query }).unwrap()
        else {
            panic!()
        };
        let titles: Vec<_> = pages.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Sunday, 22 November 2026", "Saturday, 21 November 2026"]
        );

        // Sorting by mood puts the entry without one last; the limit applies after sorting.
        let query = PageQuery {
            kind: Some(PageKind::Journal),
            conditions: vec![],
            sort: Some(Sort {
                by: journal::MOOD.into(),
                descending: false,
            }),
            limit: Some(5),
        };
        let Response::Pages { pages } = f.core.handle(Command::QueryPages { query }).unwrap()
        else {
            panic!()
        };
        let moods: Vec<_> = pages
            .iter()
            .map(|p| property(&p.properties, journal::MOOD).and_then(PropertyValue::as_number))
            .collect();
        assert_eq!(moods, [Some(1.0), Some(2.0), Some(4.0), Some(5.0), None]);
        // The Journal page itself isn't a journal entry.
        assert!(pages.iter().all(|p| p.kind == PageKind::Journal));
    }

    #[test]
    fn settings_are_checked_and_announced() {
        let mut f = fixture();
        assert_eq!(settings(&mut f.core), Settings::default());
        f.core
            .handle(Command::SetSetting {
                setting: Setting::WeekStart(0),
            })
            .unwrap();
        assert_eq!(settings(&mut f.core).week_start, 0);
        assert!(
            f.events.lock().unwrap().iter().any(
                |e| matches!(e, Event::SettingsChanged { settings } if settings.week_start == 0)
            )
        );
        assert!(
            f.core
                .handle(Command::SetSetting {
                    setting: Setting::WeekStart(7)
                })
                .is_err()
        );
        assert!(
            f.core
                .handle(Command::SetSetting {
                    setting: Setting::JournalTemplate(Some(PageId::new()))
                })
                .is_err()
        );
        // A template that is later deleted reads as none.
        let Response::PageCreated { page } = f
            .core
            .handle(Command::CreatePage {
                parent: None,
                title: "t".into(),
            })
            .unwrap()
        else {
            panic!()
        };
        f.core
            .handle(Command::SetSetting {
                setting: Setting::JournalTemplate(Some(page.id)),
            })
            .unwrap();
        f.core
            .handle(Command::DeletePage { page: page.id })
            .unwrap();
        assert_eq!(settings(&mut f.core).journal_template, None);
        // ...and a day still gets made without it.
        assert!(open_day(&mut f.core, "2026-01-01").1);
    }

    /// The commands exactly as the UI's JSON has them.
    #[test]
    fn commands_from_the_ui_parse() {
        let mut f = fixture();
        let call = |core: &mut Core, json: &str| -> Response {
            let command: Command = serde_json::from_str(json).unwrap();
            core.handle(command).unwrap()
        };
        let Response::JournalDay { page, .. } = call(
            &mut f.core,
            r#"{"type":"openJournalDay","date":"2026-11-26"}"#,
        ) else {
            panic!()
        };
        call(
            &mut f.core,
            &format!(
                r#"{{"type":"setProperty","page":"{}","name":"Mood","value":{{"type":"number","value":4}}}}"#,
                page.id
            ),
        );
        call(
            &mut f.core,
            &format!(
                r#"{{"type":"setProperty","page":"{}","name":"Mood","value":null}}"#,
                page.id
            ),
        );
        call(
            &mut f.core,
            r#"{"type":"setSetting","setting":{"type":"weekStart","value":0}}"#,
        );
        call(
            &mut f.core,
            r#"{"type":"setSetting","setting":{"type":"journalTemplate","value":null}}"#,
        );
        let Response::Pages { pages } = call(
            &mut f.core,
            r#"{"type":"queryPages","query":{"kind":"journal","conditions":[{"property":"Date","comparison":"equals","value":{"type":"date","value":"2026-11-26"}}],"sort":{"by":"Date","descending":true},"limit":10}}"#,
        ) else {
            panic!()
        };
        assert_eq!(pages.len(), 1);
        let json = serde_json::to_value(&pages[0]).unwrap();
        assert_eq!(json["kind"], "journal");
        assert_eq!(
            json["properties"],
            serde_json::json!([{"name": "Date", "value": {"type": "date", "value": "2026-11-26"}}])
        );
        assert!(
            serde_json::from_str::<Command>(r#"{"type":"openJournalDay","date":"2026-2-30"}"#)
                .is_err()
        );
    }

    #[test]
    fn properties_can_be_removed() {
        let mut f = fixture();
        let (page, _) = open_day(&mut f.core, "2026-11-26");
        set_mood(&mut f.core, page.id, 3.0);
        f.core
            .handle(Command::SetProperty {
                page: page.id,
                name: journal::MOOD.into(),
                value: None,
            })
            .unwrap();
        let (page, _) = open_day(&mut f.core, "2026-11-26");
        assert!(property(&page.properties, journal::MOOD).is_none());
        assert!(
            f.core
                .handle(Command::SetProperty {
                    page: page.id,
                    name: " ".into(),
                    value: Some(PropertyValue::Checkbox(true)),
                })
                .is_err()
        );
    }

    #[test]
    fn placeholders() {
        let d: Day = "2026-11-26".parse().unwrap();
        assert_eq!(
            fill_in("{{weekday}} {{date}} — {{title}}", d),
            "Thursday 2026-11-26 — Thursday, 26 November 2026"
        );
        assert_eq!(fill_in("{{nope}}", d), "{{nope}}");
    }
}
