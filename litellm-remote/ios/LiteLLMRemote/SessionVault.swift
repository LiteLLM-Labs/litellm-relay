import Foundation
import Security

enum SessionVault {
    static func load(workspace: String) -> String? {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "ai.litellm.remote.session", kSecAttrAccount as String: workspace,
            kSecReturnData as String: true, kSecMatchLimit as String: kSecMatchLimitOne]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess, let data = item as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func save(_ token: String, workspace: String) -> Bool {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "ai.litellm.remote.session", kSecAttrAccount as String: workspace]
        let attributes: [String: Any] = [kSecValueData as String: Data(token.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly]
        let updated = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updated == errSecSuccess { return true }
        guard updated == errSecItemNotFound else { return false }
        let added = SecItemAdd(query.merging(attributes) { _, value in value } as CFDictionary, nil)
        #if DEBUG
        if added != errSecSuccess { NSLog("Remote Keychain save failed with status %d", added) }
        #endif
        return added == errSecSuccess
    }

    static func remove(workspace: String) {
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "ai.litellm.remote.session", kSecAttrAccount as String: workspace]
        SecItemDelete(query as CFDictionary)
    }
}
