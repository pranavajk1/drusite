// SPDX-License-Identifier: AGPL-3.0-or-later

// The store key in a browser: 32 random bytes, as on every device, kept in IndexedDB wrapped
// (AES-GCM) by a WebCrypto key that is itself kept there but can't be exported. Script on the
// page can use it; nothing can read it out, and a copy of the profile's files is ciphertext.

const DB = "drusite";
const STORE = "keys";
const NAME = "store";

interface Kept {
  wrapper: CryptoKey;
  iv: Uint8Array<ArrayBuffer>;
  wrapped: ArrayBuffer;
}

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function run<T>(db: IDBDatabase, mode: IDBTransactionMode, f: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, mode);
    const request = f(tx.objectStore(STORE));
    tx.oncomplete = () => resolve(request.result);
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
  });
}

/** The store key, made the first time; `fresh` when it was made just now. */
export async function storeKey(): Promise<{ key: Uint8Array; fresh: boolean }> {
  const db = await open();
  try {
    const kept = await run<Kept | undefined>(db, "readonly", (s) => s.get(NAME));
    if (kept) {
      const raw = await crypto.subtle.decrypt({ name: "AES-GCM", iv: kept.iv }, kept.wrapper, kept.wrapped);
      return { key: new Uint8Array(raw), fresh: false };
    }
    const key = crypto.getRandomValues(new Uint8Array(32));
    const wrapper = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const wrapped = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, wrapper, key);
    await run(db, "readwrite", (s) => s.put({ wrapper, iv, wrapped } satisfies Kept, NAME));
    return { key, fresh: true };
  } finally {
    db.close();
  }
}

/** Forgets the key (signing out): the store it opened is unreadable from then on. */
export async function forgetKey(): Promise<void> {
  const db = await open();
  try {
    await run(db, "readwrite", (s) => s.delete(NAME));
  } finally {
    db.close();
  }
}
