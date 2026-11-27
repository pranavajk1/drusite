// SPDX-License-Identifier: AGPL-3.0-or-later

import AuthenticationServices
import Foundation
import Security
import SwiftRs
import Tauri
import UIKit
import WebKit

class AliasArgs: Decodable { let alias: String }
class SignArgs: Decodable {
  let alias: String
  let message: String
}
class AuthenticateArgs: Decodable {
  let url: String
  let scheme: String
}

/// The iOS half of the Drusite plugin: the device key in the Secure Enclave, signing in with
/// ASWebAuthenticationSession, and a keyboard without WKWebView's accessory bar.
class DrusitePlugin: Plugin, ASWebAuthenticationPresentationContextProviding {
  private var session: ASWebAuthenticationSession?
  private var pending: Invoke?

  /// Swiping in from the left edge goes back, as in Notion's and other iOS apps: the app keeps
  /// its pages and what's open over them (sheets, menus, Settings sections) in the webview's
  /// history, so the swipe closes what's open, then goes back a page (`ui/src/lib/back.ts`).
  ///
  /// Hides WKWebView's form accessory bar (the ^ ⌄ ✓ strip above the keyboard), which native
  /// editors don't show, by giving the web content view a runtime subclass whose
  /// `inputAccessoryView` is nil. A common trick, but it depends on WebKit's private
  /// class-name prefix ("WKContent"); if that changes, the bar simply comes back.
  override func load(webview: WKWebView) {
    webview.allowsBackForwardNavigationGestures = true
    guard
      let content = webview.scrollView.subviews.first(where: {
        String(describing: type(of: $0)).hasPrefix("WKContent")
      }),
      let base: AnyClass = object_getClass(content)
    else { return }
    let name = "\(base)_DrusiteNoAccessory"
    var sub: AnyClass? = NSClassFromString(name)
    if sub == nil, let made = objc_allocateClassPair(base, name, 0) {
      let none: @convention(block) (AnyObject) -> UIView? = { _ in nil }
      class_addMethod(
        made, #selector(getter: UIResponder.inputAccessoryView), imp_implementationWithBlock(none),
        "@@:")
      objc_registerClassPair(made)
      sub = made
    }
    if let sub { object_setClass(content, sub) }
  }

  // MARK: Device key

  private func tag(_ alias: String) -> Data { "dev.drusite.dpop.\(alias)".data(using: .utf8)! }

  private func find(_ alias: String) -> SecKey? {
    let query: [String: Any] = [
      kSecClass as String: kSecClassKey,
      kSecAttrApplicationTag as String: tag(alias),
      kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
      kSecReturnRef as String: true,
    ]
    var item: CFTypeRef?
    guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let key = item else {
      return nil
    }
    return (key as! SecKey)
  }

  private func describe(_ key: SecKey) throws -> [String: Any] {
    guard let pub = SecKeyCopyPublicKey(key) else {
      throw NSError(
        domain: "drusite", code: 1, userInfo: [NSLocalizedDescriptionKey: "no public key"])
    }
    var err: Unmanaged<CFError>?
    guard let raw = SecKeyCopyExternalRepresentation(pub, &err) as Data? else {
      throw err!.takeRetainedValue() as Error
    }
    let attrs = SecKeyCopyAttributes(key) as? [String: Any] ?? [:]
    let token = attrs[kSecAttrTokenID as String] as? String
    return [
      "publicKey": raw.base64EncodedString(),
      "hardware": token == (kSecAttrTokenIDSecureEnclave as String),
    ]
  }

  /// The P-256 key for `alias`, made on first use: in the Secure Enclave, usable once the
  /// phone has been unlocked after boot, never leaving this device. Where there is no enclave
  /// (older simulators), an ordinary this-device-only keychain key.
  @objc public func deviceKey(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(AliasArgs.self)
    if let key = find(args.alias) {
      invoke.resolve(try describe(key))
      return
    }
    var err: Unmanaged<CFError>?
    guard
      let access = SecAccessControlCreateWithFlags(
        nil, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, .privateKeyUsage, &err)
    else {
      invoke.reject("access control: \(err!.takeRetainedValue())")
      return
    }
    var attrs: [String: Any] = [
      kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
      kSecAttrKeySizeInBits as String: 256,
      kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
      kSecPrivateKeyAttrs as String: [
        kSecAttrIsPermanent as String: true,
        kSecAttrApplicationTag as String: tag(args.alias),
        kSecAttrAccessControl as String: access,
      ] as [String: Any],
    ]
    var key = SecKeyCreateRandomKey(attrs as CFDictionary, &err)
    if key == nil {
      attrs.removeValue(forKey: kSecAttrTokenID as String)
      attrs[kSecPrivateKeyAttrs as String] =
        [
          kSecAttrIsPermanent as String: true,
          kSecAttrApplicationTag as String: tag(args.alias),
          kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ] as [String: Any]
      key = SecKeyCreateRandomKey(attrs as CFDictionary, &err)
    }
    guard let made = key else {
      invoke.reject("can't make the device key: \(err!.takeRetainedValue())")
      return
    }
    invoke.resolve(try describe(made))
  }

  /// An ECDSA P-256 signature (DER) over the base64 `message`.
  @objc public func sign(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SignArgs.self)
    guard let key = find(args.alias) else {
      invoke.reject("no device key")
      return
    }
    guard let message = Data(base64Encoded: args.message) else {
      invoke.reject("the message isn't base64")
      return
    }
    var err: Unmanaged<CFError>?
    guard
      let signature = SecKeyCreateSignature(
        key, .ecdsaSignatureMessageX962SHA256, message as CFData, &err) as Data?
    else {
      invoke.reject("can't sign: \(err!.takeRetainedValue())")
      return
    }
    invoke.resolve(["signature": signature.base64EncodedString()])
  }

  @objc public func deviceName(_ invoke: Invoke) throws {
    // UIDevice.name is only "iPhone" without a special entitlement, which is honest enough.
    DispatchQueue.main.async {
      invoke.resolve(["name": "\(UIDevice.current.name) (\(UIDevice.current.systemName))"])
    }
  }

  // MARK: Signing in

  func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    return scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? ASPresentationAnchor()
  }

  /// Opens `url` in a sign-in sheet that shares Safari's cookies and passkeys, and resolves with
  /// the URL it was sent back to on `scheme`.
  @objc public func authenticate(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(AuthenticateArgs.self)
    guard let url = URL(string: args.url) else {
      invoke.reject("not a URL: \(args.url)")
      return
    }
    DispatchQueue.main.async {
      self.session?.cancel()
      self.pending?.resolve(["cancelled": true])
      self.pending = invoke
      let session = ASWebAuthenticationSession(url: url, callbackURLScheme: args.scheme) {
        callback, error in
        DispatchQueue.main.async {
          guard let pending = self.pending else { return }
          self.pending = nil
          self.session = nil
          if let callback {
            pending.resolve(["url": callback.absoluteString])
          } else if let error = error as? ASWebAuthenticationSessionError,
            error.code == .canceledLogin
          {
            pending.resolve(["cancelled": true])
          } else {
            pending.reject("sign-in failed: \(error?.localizedDescription ?? "no answer")")
          }
        }
      }
      session.presentationContextProvider = self
      session.prefersEphemeralWebBrowserSession = false
      self.session = session
      if !session.start() {
        self.pending = nil
        self.session = nil
        invoke.reject("can't open the sign-in sheet")
      }
    }
  }

  @objc public func cancelAuthentication(_ invoke: Invoke) throws {
    DispatchQueue.main.async {
      self.session?.cancel()
      self.session = nil
      self.pending?.resolve(["cancelled": true])
      self.pending = nil
      invoke.resolve()
    }
  }
}

@_cdecl("init_plugin_drusite")
public func initPlugin() -> Plugin {
  return DrusitePlugin()
}
