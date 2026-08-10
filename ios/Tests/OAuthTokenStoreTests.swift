import Foundation
import Testing
@testable import ClippySyncCore

@Test func oauthTokensPersistAcrossStoreInstancesWithoutKeychain() throws {
    let directory = FileManager.default.temporaryDirectory
        .appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: directory) }
    let expected = OAuthSessionTokens(
        accessToken: "access",
        idToken: "identity",
        refreshToken: "refresh"
    )

    try OAuthTokenStore(environment: "production", baseDirectory: directory).save(expected)
    let restored = try OAuthTokenStore(
        environment: "production",
        baseDirectory: directory
    ).load()

    #expect(restored == expected)
    #expect(try OAuthTokenStore(environment: "staging", baseDirectory: directory).load() == nil)
}

@Test func oauthTokenStoreDeletesTheSession() throws {
    let directory = FileManager.default.temporaryDirectory
        .appending(path: UUID().uuidString, directoryHint: .isDirectory)
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = OAuthTokenStore(environment: "production", baseDirectory: directory)
    try store.save(OAuthSessionTokens(accessToken: "a", idToken: "i", refreshToken: nil))

    try store.delete()

    #expect(try store.load() == nil)
}
