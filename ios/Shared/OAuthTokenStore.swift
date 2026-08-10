import Foundation

public struct OAuthSessionTokens: Codable, Equatable, Sendable {
    public var accessToken: String
    public var idToken: String
    public var refreshToken: String?

    public init(accessToken: String, idToken: String, refreshToken: String?) {
        self.accessToken = accessToken
        self.idToken = idToken
        self.refreshToken = refreshToken
    }
}

/// Persists OAuth credentials in Clippy's sandbox instead of Keychain. The
/// file is environment-scoped, excluded from backups, written atomically, and
/// protected by iOS until the device has been unlocked after boot. Reading it
/// never presents a password or biometric prompt.
public struct OAuthTokenStore: Sendable {
    private static let maximumBytes = 200_000
    private let fileURL: URL

    public init(environment: String, baseDirectory: URL? = nil) {
        let root = baseDirectory ?? FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appending(path: "Clippy", directoryHint: .isDirectory)
        fileURL = root.appending(
            path: "oauth-session-\(environment).json",
            directoryHint: .notDirectory
        )
    }

    public func load() throws -> OAuthSessionTokens? {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return nil }
        let data = try Data(contentsOf: fileURL, options: .uncached)
        guard data.count <= Self.maximumBytes else { throw OAuthTokenStoreError.invalidSession }
        let session: OAuthSessionTokens
        do { session = try JSONDecoder().decode(OAuthSessionTokens.self, from: data) }
        catch { throw OAuthTokenStoreError.invalidSession }
        try validate(session)
        return session
    }

    public func save(_ session: OAuthSessionTokens) throws {
        try validate(session)
        let manager = FileManager.default
        let directory = fileURL.deletingLastPathComponent()
        try manager.createDirectory(
            at: directory,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        let data = try JSONEncoder().encode(session)
        guard data.count <= Self.maximumBytes else { throw OAuthTokenStoreError.invalidSession }
        var options: Data.WritingOptions = [.atomic]
        #if os(iOS)
        options.insert(.completeFileProtectionUntilFirstUserAuthentication)
        #endif
        try data.write(to: fileURL, options: options)
        try manager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: fileURL.path)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableURL = fileURL
        try mutableURL.setResourceValues(values)
    }

    public func delete() throws {
        do { try FileManager.default.removeItem(at: fileURL) }
        catch let error as CocoaError where error.code == .fileNoSuchFile { return }
    }

    private func validate(_ session: OAuthSessionTokens) throws {
        guard !session.accessToken.isEmpty,
              !session.idToken.isEmpty,
              session.accessToken.utf8.count <= 65_536,
              session.idToken.utf8.count <= 65_536,
              session.refreshToken?.isEmpty != true,
              (session.refreshToken?.utf8.count ?? 0) <= 65_536 else {
            throw OAuthTokenStoreError.invalidSession
        }
    }
}

public enum OAuthTokenStoreError: Error, Equatable, Sendable {
    case invalidSession
}
