import Foundation
import Security
import Testing
@testable import ClippySyncCore

@Test func concurrentContentIsNeverSilentlyLost() {
    var register = ContentRegister()
    register.apply(ContentVersion(
        dot: Dot(actorId: "mac", counter: 1),
        context: VersionVector(),
        value: "Mac edit"
    ))
    register.apply(ContentVersion(
        dot: Dot(actorId: "phone", counter: 1),
        context: VersionVector(),
        value: "Phone edit"
    ))

    #expect(register.hasConflict)
    #expect(Set(register.versions.map(\.value)) == ["Mac edit", "Phone edit"])
}

@Test func aResolutionObservesAndReplacesAllVariants() {
    var register = ContentRegister()
    let mac = Dot(actorId: "mac", counter: 1)
    let phone = Dot(actorId: "phone", counter: 1)
    register.apply(ContentVersion(dot: mac, context: VersionVector(), value: "M"))
    register.apply(ContentVersion(dot: phone, context: VersionVector(), value: "P"))
    register.apply(ContentVersion(
        dot: Dot(actorId: "phone", counter: 2),
        context: VersionVector(["mac": 1, "phone": 1]),
        value: "Resolved"
    ))

    #expect(!register.hasConflict)
    #expect(register.projectedValue == "Resolved")
}

@Test func fileManifestDetectsTampering() throws {
    let original = Data("abcdefghij".utf8)
    let manifest = try FileManifest.make(data: original, chunkSize: 4)

    #expect(manifest.chunks.count == 3)
    #expect(manifest.verify(reconstructed: original))
    #expect(!manifest.verify(reconstructed: Data("abcdefghiX".utf8)))
}

@Test func schedulerUsesBoundedForegroundBurstAndHiddenBackoff() {
    var foreground = BackoffSchedule(visibility: .foreground)
    #expect(foreground.nextDelay(jitterUnit: 0.5, hasLocalOperations: false) == 0)
    #expect(foreground.nextDelay(jitterUnit: 0.5, hasLocalOperations: false) == 0.25)
    #expect(foreground.nextDelay(jitterUnit: 0.5, hasLocalOperations: false) == 1)
    #expect(foreground.nextDelay(jitterUnit: 0.5, hasLocalOperations: false) == 3)

    var hidden = BackoffSchedule(visibility: .hidden)
    for _ in 0..<20 { hidden.failed() }
    #expect(hidden.nextDelay(jitterUnit: 0.5, hasLocalOperations: false) == 900)
    #expect(hidden.nextDelay(jitterUnit: 0.5, hasLocalOperations: true) == 0)
}

@Test func sealedPayloadAuthenticatesItsContext() throws {
    let key = try WorkspaceKey(data: Data(repeating: 9, count: 32))
    let envelope = try SyncCrypto.seal(Data("private".utf8), key: key, aad: Data("workspace-1".utf8))
    let opened = try SyncCrypto.open(envelope, key: key, aad: Data("workspace-1".utf8))
    #expect(opened == Data("private".utf8))
    #expect(throws: SyncCryptoError.authenticationFailed) {
        try SyncCrypto.open(envelope, key: key, aad: Data("workspace-2".utf8))
    }
}

@Test func localReplicaPersistsIdentityFrontierAndPendingOperations() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }

    let first = try LocalSyncStore(workspaceId: "workspace-persistence", baseDirectory: directory)
    let initialActor = await first.view().actorId
    let section = try await first.createSection(name: "Inbox")
    let item = try await first.createItem(sectionId: section.id, content: "Durable note")
    let beforeRestart = await first.view()

    let reopened = try LocalSyncStore(workspaceId: "workspace-persistence", baseDirectory: directory)
    let afterRestart = await reopened.view()

    #expect(afterRestart.actorId == initialActor)
    #expect(afterRestart.pendingOperationCount == beforeRestart.pendingOperationCount)
    #expect(afterRestart.sections.map(\.id) == [section.id])
    #expect(afterRestart.items.first(where: { $0.id == item.id })?.projectedContent == "Durable note")
    let payload = try await reopened.outboundPayload()
    #expect(payload.frontier.counters[initialActor] == UInt64(beforeRestart.pendingOperationCount))
}

@Test func outboundPageNeverAdvertisesAnUnsentLocalDot() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try LocalSyncStore(workspaceId: "workspace-pagination", baseDirectory: directory)
    let section = try await store.createSection(name: "Initial")
    for index in 0..<1_999 {
        try await store.renameSection(id: section.id, name: "Page \(index)")
    }

    let view = await store.view()
    let firstPage = try await store.outboundPayload()
    #expect(view.pendingOperationCount == 2_001)
    #expect(firstPage.operations.count == 2_000)
    #expect(firstPage.operations.last?.dot.counter == 2_000)
    #expect(firstPage.frontier.counters[view.actorId] == 2_000)

    let completePage = try await store.outboundPayload(limit: 2_001)
    #expect(completePage.frontier.counters[view.actorId] == 2_001)
}

@Test func remoteApplyIsIdempotentAndConcurrentContentSurvivesRestart() async throws {
    let macDirectory = temporaryDirectory()
    let phoneDirectory = temporaryDirectory()
    defer {
        try? FileManager.default.removeItem(at: macDirectory)
        try? FileManager.default.removeItem(at: phoneDirectory)
    }
    let workspace = "workspace-conflict"
    let mac = try LocalSyncStore(workspaceId: workspace, baseDirectory: macDirectory)
    let phone = try LocalSyncStore(workspaceId: workspace, baseDirectory: phoneDirectory)

    let section = try await mac.createSection(name: "Shared")
    let item = try await mac.createItem(sectionId: section.id, content: "Base")
    let bootstrap = try await mac.outboundPayload()
    let firstApply = try await phone.applyRemotePayload(bootstrap)
    let duplicateApply = try await phone.applyRemotePayload(bootstrap)
    #expect(firstApply.appliedOperationCount == bootstrap.operations.count)
    #expect(duplicateApply.appliedOperationCount == 0)

    // Return the phone frontier so the Mac can durably acknowledge bootstrap.
    _ = try await mac.applyRemotePayload(try await phone.outboundPayload())
    try await mac.updateItem(id: item.id, content: "Edit from Mac")
    try await phone.updateItem(id: item.id, content: "Edit from iPhone")
    _ = try await mac.applyRemotePayload(try await phone.outboundPayload())

    let conflicted = await mac.view().items.first(where: { $0.id == item.id })
    #expect(conflicted?.content.hasConflict == true)
    #expect(Set(conflicted?.content.versions.map(\.value) ?? []) == ["Edit from Mac", "Edit from iPhone"])

    let reopened = try LocalSyncStore(workspaceId: workspace, baseDirectory: macDirectory)
    let durableConflict = await reopened.view().items.first(where: { $0.id == item.id })
    #expect(durableConflict?.content.hasConflict == true)
}

@Test func metadataLwwConvergesWhenOperationsArriveOutOfOrder() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let workspace = "workspace-lww"
    let store = try LocalSyncStore(workspaceId: workspace, baseDirectory: directory)
    let id = UUID()
    let newer = SyncOperation(
        schemaVersion: 1,
        workspaceId: workspace,
        entityKind: "section",
        entityId: id.uuidString,
        dot: Dot(actorId: "peer", counter: 2),
        mutation: .setMetadata(field: "name", value: .string("Newer"))
    )
    let older = SyncOperation(
        schemaVersion: 1,
        workspaceId: workspace,
        entityKind: "section",
        entityId: id.uuidString,
        dot: Dot(actorId: "peer", counter: 1),
        mutation: .setMetadata(field: "name", value: .string("Older"))
    )
    _ = try await store.applyRemotePayload(SyncPayload(
        workspaceId: workspace,
        frontier: VersionVector(["peer": 2]),
        operations: [newer, older]
    ))
    #expect(await store.view().sections.first?.name == "Newer")
}

@Test func replicaRejectsCausalGapsInsteadOfInventingObservedHistory() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let workspace = "workspace-gap"
    let store = try LocalSyncStore(workspaceId: workspace, baseDirectory: directory)
    let operation = SyncOperation(
        schemaVersion: 1,
        workspaceId: workspace,
        entityKind: "section",
        entityId: UUID().uuidString,
        dot: Dot(actorId: "peer", counter: 2),
        mutation: .setMetadata(field: "name", value: .string("Missing predecessor"))
    )
    let payload = SyncPayload(
        workspaceId: workspace,
        frontier: VersionVector(["peer": 2]),
        operations: [operation]
    )
    await #expect(throws: LocalSyncStoreError.causalGap) {
        try await store.applyRemotePayload(payload)
    }
    #expect(await store.view().sections.isEmpty)
}

@Test func attachmentChunksAreEncryptedDurableAndVerified() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let workspace = "workspace-files"
    let key = try WorkspaceKey(data: Data(repeating: 7, count: 32))
    let plaintext = Data("private attachment bytes".utf8)
    let store = try LocalSyncStore(workspaceId: workspace, baseDirectory: directory)
    let section = try await store.createSection(name: "Files")
    let item = try await store.createItem(sectionId: section.id, content: "Attachment")
    let attachment = try await store.addAttachment(
        itemId: item.id,
        name: "note.txt",
        mediaType: "text/plain",
        data: plaintext,
        key: key,
        chunkSize: 5
    )

    let chunkFiles = try FileManager.default
        .subpathsOfDirectory(atPath: directory.path)
        .filter { $0.hasSuffix(".chunk") }
    #expect(!chunkFiles.isEmpty)
    for path in chunkFiles {
        let stored = try Data(contentsOf: directory.appending(path: path))
        #expect(!stored.contains(plaintext))
    }
    #expect(try await store.reconstructAttachment(id: attachment.id, key: key) == plaintext)

    let reopened = try LocalSyncStore(workspaceId: workspace, baseDirectory: directory)
    #expect(try await reopened.reconstructAttachment(id: attachment.id, key: key) == plaintext)
}

@Test func remoteAttachmentManifestCannotExceedMobileLimit() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let workspace = "workspace-attachment-limit"
    let operation = SyncOperation(
        schemaVersion: 1,
        workspaceId: workspace,
        entityKind: "attachment",
        entityId: UUID().uuidString,
        dot: Dot(actorId: UUID().uuidString.lowercased(), counter: 1),
        mutation: .setMetadata(field: "manifest", value: .object([
            "schemaVersion": .number(1),
            "fileSha256": .string(String(repeating: "0", count: 64)),
            "size": .number(Double(LocalSyncStore.maxAttachmentBytes + 1)),
            "chunkSize": .number(1_048_576),
            "chunks": .array([])
        ]))
    )
    let store = try LocalSyncStore(workspaceId: workspace, baseDirectory: directory)

    await #expect(throws: LocalSyncStoreError.invalidManifest) {
        try await store.applyRemotePayload(SyncPayload(
            workspaceId: workspace,
            frontier: VersionVector([operation.dot.actorId: 1]),
            operations: [operation]
        ))
    }
}

@Test func rustWireGoldenJSONDecodesAndProjectsCanonicalFields() async throws {
    let directory = temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let fixtureURL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .deletingLastPathComponent()
        .appending(path: "fixtures/sync-wire-v1.json")
    let payload = try JSONDecoder().decode(
        SyncPayload.self,
        from: Data(contentsOf: fixtureURL)
    )
    let workspace = payload.workspaceId
    let sectionId = UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    let itemId = UUID(uuidString: "33333333-3333-4333-8333-333333333333")!
    let createdAtMs: UInt64 = 1_786_276_800_000
    let fileHash = String(repeating: "0", count: 64)
    let chunkHash = String(repeating: "a", count: 64)
    let store = try LocalSyncStore(workspaceId: workspace, baseDirectory: directory)
    _ = try await store.applyRemotePayload(payload)
    let view = await store.view()

    #expect(view.sections.first?.name == "Inbox")
    #expect(view.items.first?.sectionId == sectionId)
    #expect(view.items.first?.createdAt == createdAtMs)
    #expect(view.items.first?.done == true)
    #expect(view.items.first?.projectedContent == "From Rust")
    #expect(view.attachments.first?.itemId == itemId)
    #expect(view.attachments.first?.name == "note.txt")
    #expect(view.attachments.first?.size == 4)
    #expect(view.attachments.first?.manifest?.fileSha256 == fileHash)
    #expect(SyncCrypto.payloadAAD(workspaceId: workspace) == lengthPrefixed(["clippy-sync-payload", "1", workspace]))
    #expect(SyncCrypto.chunkAAD(workspaceId: workspace, hash: chunkHash) == lengthPrefixed(["clippy-sync-chunk", "1", workspace, chunkHash]))
    #expect(SyncCrypto.payloadAAD(workspaceId: "workspace-123").testHex == "00000013636c697070792d73796e632d7061796c6f616400000001310000000d776f726b73706163652d313233")
}

@Test func jwtVerifierAcceptsOnlyAValidRS256SignatureAndClaims() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let fixture = try JWTFixture()
    let verifier = try JWTVerifier(
        issuer: fixture.issuer,
        audience: fixture.audience,
        loader: StaticJWKSLoader(data: fixture.jwks),
        now: { now }
    )
    let token = try fixture.token(expiresAt: now.addingTimeInterval(300), nonce: "nonce-1")
    let claims = try await verifier.verify(token, kind: .id, expectedNonce: "nonce-1")

    #expect(claims.subject == "user-123")
    #expect(claims.organizationId == "org-123")
    #expect(claims.expiresAt == now.addingTimeInterval(300))
}

@Test func jwtVerifierRejectsForgedSignature() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let trusted = try JWTFixture()
    let verifier = try JWTVerifier(
        issuer: trusted.issuer,
        audience: trusted.audience,
        loader: StaticJWKSLoader(data: trusted.jwks),
        now: { now }
    )
    let signed = try trusted.token(expiresAt: now.addingTimeInterval(300))
    var segments = signed.split(separator: ".").map(String.init)
    var signature = try Data(base64URLEncoded: segments[2])
    signature[signature.startIndex] ^= 1
    segments[2] = signature.base64URLEncodedString()
    let forged = segments.joined(separator: ".")

    await #expect(throws: JWTVerificationError.invalidSignature) {
        try await verifier.verify(forged)
    }
}

@Test func jwtVerifierRejectsWrongIssuer() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let fixture = try JWTFixture()
    let verifier = try fixture.verifier(now: now)
    let token = try fixture.token(
        issuer: "https://wrong.example.com",
        expiresAt: now.addingTimeInterval(300)
    )

    await #expect(throws: JWTVerificationError.wrongIssuer) {
        try await verifier.verify(token)
    }
}

@Test func jwtVerifierRejectsWrongAudience() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let fixture = try JWTFixture()
    let verifier = try fixture.verifier(now: now)
    let token = try fixture.token(
        audience: "wrong-audience",
        expiresAt: now.addingTimeInterval(300)
    )

    await #expect(throws: JWTVerificationError.wrongAudience) {
        try await verifier.verify(token, kind: .id)
    }
}

@Test func jwtVerifierRejectsAccessTokenFromAnotherApplication() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let fixture = try JWTFixture()
    let verifier = try fixture.verifier(now: now)
    let token = try fixture.token(
        clientId: "wrong-client",
        expiresAt: now.addingTimeInterval(300)
    )

    await #expect(throws: JWTVerificationError.wrongAudience) {
        try await verifier.verify(token)
    }
}

@Test func jwtVerifierRejectsExpiredToken() async throws {
    let now = Date(timeIntervalSince1970: 2_000_000_000)
    let fixture = try JWTFixture()
    let verifier = try fixture.verifier(now: now)
    let token = try fixture.token(expiresAt: now.addingTimeInterval(-1))

    await #expect(throws: JWTVerificationError.expired) {
        try await verifier.verify(token)
    }
}

private func temporaryDirectory() -> URL {
    FileManager.default.temporaryDirectory
        .appending(path: "clippy-sync-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
}

private func lengthPrefixed(_ fields: [String]) -> Data {
    var result = Data()
    for field in fields {
        let bytes = Data(field.utf8)
        var length = UInt32(bytes.count).bigEndian
        withUnsafeBytes(of: &length) { result.append(contentsOf: $0) }
        result.append(bytes)
    }
    return result
}

private extension Data {
    var testHex: String { map { String(format: "%02x", $0) }.joined() }
}

private struct StaticJWKSLoader: JWKSLoading {
    let data: Data

    func loadJWKS(from url: URL) async throws -> Data { data }
}

private struct JWTFixture {
    let issuer = URL(string: "https://auth.example.com")!
    let audience = "client-123"
    let privateKey: SecKey
    let jwks: Data

    init() throws {
        guard let keyData = Data(base64Encoded: Self.privateKeyBase64) else {
            throw JWTFixtureError.keyGeneration
        }
        let attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeRSA,
            kSecAttrKeyClass as String: kSecAttrKeyClassPrivate,
            kSecAttrKeySizeInBits as String: 2_048
        ]
        var error: Unmanaged<CFError>?
        guard let privateKey = SecKeyCreateWithData(
                keyData as CFData,
                attributes as CFDictionary,
                &error
              ),
              let publicKey = SecKeyCopyPublicKey(privateKey),
              let external = SecKeyCopyExternalRepresentation(publicKey, &error) as Data? else {
            _ = error?.takeRetainedValue()
            throw JWTFixtureError.keyGeneration
        }
        self.privateKey = privateKey
        let components = try RSAKeyDER.jwkComponents(pkcs1PublicKey: external)
        jwks = try JSONSerialization.data(withJSONObject: [
            "keys": [[
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": "test-key",
                "n": components.n,
                "e": components.e
            ]]
        ])
    }

    // A non-production PKCS#1 fixture key used only to create deterministic
    // Security.framework test signatures without touching the Keychain.
    private static let privateKeyBase64 = "MIIEpAIBAAKCAQEAt0l4ipk1vQfeNniybbLOe8RqimBHUipk7zBernCDTN9RrDJ7GyATC+pWbXYjs0fqqvFK5ND/mebaVpr255CMgDHED/2X8Nu5SSkZ63AZZU2jpoCare0wfGdmTmyfCSZl8Dp7yXqZx0fDTzl7bSZ4RQ4n8Muw20DmhuxLHgxzaxMYz6APv1+LHThSVGiOBnmho3exPSeFXC7CMvQXoIAB6wsuYYjM+ohMdqOYc99wU/Cu0dkT68dAmWHDSgo12+JFY82z8Kc5GgnCjY7RDe3eZ3V5njgwemHYC2SXMqcXtMRzL0plZcd1dC7YkxSJcPvT6HIGH/tUhmlY0S96MNbrDQIDAQABAoIBAAV9+W/Ph+h5ezbkahYW94lp4faAaPDqdoNcTpLiODQY0W3qYqgu+hToNc84iXjgPMwfZJysav/bIz2DxsY65JrjqfnjWsNLwG3MkGC1496yyKaUXiqflKLMx0SuUzhOTZSOUy7NQI7T5o4Y9GdnxwezlRyUemDbEo1NVRWPa0ifJW92Ph8xSkS3cIA9uYy8YWv9N/Y1k+PtzhS0RP7FV4e5IdM65l0ryq2aHd/t4gH4cyjEiGtBElWQ9eUFVR+nuKefaUZctVV5SsiuD78mONbIbxTJerZbVVA8bUwrnjjmAmyU1ghVxiSGCUMUKbo60f8dJicTOs2MZHINqM/WdtkCgYEA+DhhucxE2LnxDCX4V8QzXSYutVn92CDrcM0mQOXCLZ0C/A3AtA3pfkwCYfv6FCWaQl3Vj/yA6p+oDo+BDiTDkgtyPsPBR7bVJJxR0g75SI5EngDrYmClMw0RlZ+R3WAvmDFcqxL9/o2WViZjL7amiBlDpYtqjNAdY63ae/8prwMCgYEAvQgXVvNEkcvz8fOwQszml0r4G8yKPye/n4TXnJGXJTFLlGX+33gk7ElZaJueE0jt9We982CrFgtfZIQAMweD0f45y2NVVvzGrLD2I9TldwoC/Q84awwsZHLQW7xGBMHSnnTTzq5L792tzP2VlHR5pt4nWLNTCKze6ywnZg5QGK8CgYEAi0vIu+XANQeUGEcuqMI4OOv2hlssMx+2QKU/9Gd7ovFb/WsSW3j7MZ8iLy6i1q+Lc/cIpDcFeaWDQDiUKgXDoq+9uy9Lxhz6XANFf2ZbyrXcF/dYIOsvigipd5gG2X7i9rusz2xnEXUPiuUcAGi15+aVqc8lSkR4Wbn0xGUbVVUCgYEAg/h0IvYvdwJGyzJwahKXIiTq1q2UDsd3Vqztwpc6SHMD6xTSPb2pOXV0AD40vA38Y4oL6TAiAX/rF0e4w+eJNkAgpUgyOkq7gbECBr4JfXP15iqMHuAe1fn6UTE+SO/wVUQG45J33XyMbELV/RDcJY2PNrPrUnEuKE1pLCzt6m0CgYBcJAnuJ6ZEgZ7e2nGfMxuYLJSAKFVYLcn1stdqXhfARw7cugud3P1wgrLJQZb9GesB+QguSnhyYWA2/lzAmIaGNnUItK7PsnmkDGczhrz6YhKFRABlZCEm0pRUSTFEz3UdKlnUjdbFFyFP9brmFsaKtmzZgMIKRdBv1umsg67KeQ=="

    func verifier(now: Date) throws -> JWTVerifier {
        try JWTVerifier(
            issuer: issuer,
            audience: audience,
            loader: StaticJWKSLoader(data: jwks),
            now: { now }
        )
    }

    func token(
        issuer: String? = nil,
        audience: String? = nil,
        clientId: String? = nil,
        expiresAt: Date,
        nonce: String? = nil
    ) throws -> String {
        let header = try JSONSerialization.data(withJSONObject: [
            "alg": "RS256", "kid": "test-key", "typ": "JWT"
        ])
        var claims: [String: Any] = [
            "iss": issuer ?? self.issuer.absoluteString,
            "aud": audience ?? self.audience,
            "client_id": clientId ?? self.audience,
            "exp": expiresAt.timeIntervalSince1970,
            "sub": "user-123",
            "org_id": "org-123"
        ]
        if let nonce { claims["nonce"] = nonce }
        let payload = try JSONSerialization.data(withJSONObject: claims)
        let signingInput = "\(header.base64URLEncodedString()).\(payload.base64URLEncodedString())"
        var error: Unmanaged<CFError>?
        guard let signature = SecKeyCreateSignature(
            privateKey,
            .rsaSignatureMessagePKCS1v15SHA256,
            Data(signingInput.utf8) as CFData,
            &error
        ) as Data? else {
            _ = error?.takeRetainedValue()
            throw JWTFixtureError.signing
        }
        return "\(signingInput).\(signature.base64URLEncodedString())"
    }
}

private enum JWTFixtureError: Error {
    case keyGeneration
    case signing
}
