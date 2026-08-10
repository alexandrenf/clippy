import CryptoKit
import Foundation

public struct PairingOffer: Codable, Equatable, Sendable {
    public let version: UInt8
    public let workspaceId: String
    public let tunnelUrl: String
    public let workosIssuer: String
    public let workosAudience: String
    public let macPublicKey: String
    public let oneTimeToken: String
    public let expiresAtMs: UInt64
}

public struct PairingResponse: Codable, Equatable, Sendable {
    public let phonePublicKey: String
    public let oneTimeToken: String
}

public struct PairingGrant: Codable, Equatable, Sendable {
    public let macPublicKey: String
    public let phonePublicKey: String
    public let sealedWorkspace: SealedEnvelope
}

public struct AuthenticatedPrincipal: Equatable, Sendable {
    public let subject: String
    public let organizationId: String?

    public init(subject: String, organizationId: String?) {
        self.subject = subject
        self.organizationId = organizationId
    }
}

public struct WorkspaceKey: Equatable, Sendable {
    public let data: Data

    public init(data: Data) throws {
        guard data.count == 32 else { throw SyncCryptoError.invalidKey }
        self.data = data
    }

    public init(base64URL: String) throws {
        try self.init(data: Data(base64URLEncoded: base64URL))
    }

    public var base64URL: String { data.base64URLEncodedString() }
}

public struct SealedEnvelope: Codable, Equatable, Sendable {
    public let version: UInt8
    public let nonce: String
    public let ciphertext: String
}

public enum SyncCrypto {
    public struct PhonePairing: Sendable {
        public let response: PairingResponse
        fileprivate let privateKey: Curve25519.KeyAgreement.PrivateKey

        public init(offer: PairingOffer) {
            let privateKey = Curve25519.KeyAgreement.PrivateKey()
            self.privateKey = privateKey
            response = PairingResponse(
                phonePublicKey: privateKey.publicKey.rawRepresentation.base64URLEncodedString(),
                oneTimeToken: offer.oneTimeToken
            )
        }

        public func unwrap(
            grant: PairingGrant,
            offer: PairingOffer,
            principal: AuthenticatedPrincipal
        ) throws -> WorkspaceKey {
            guard grant.macPublicKey == offer.macPublicKey,
                  grant.phonePublicKey == response.phonePublicKey else {
                throw SyncCryptoError.authenticationFailed
            }
            let macKey = try Curve25519.KeyAgreement.PublicKey(
                rawRepresentation: Data(base64URLEncoded: grant.macPublicKey)
            )
            let shared = try privateKey.sharedSecretFromKeyAgreement(with: macKey)
            let wrapKey = shared.hkdfDerivedSymmetricKey(
                using: SHA256.self,
                salt: Data(offer.oneTimeToken.utf8),
                sharedInfo: Data("clippy-sync-pairing-wrap-key-v1:\(offer.workspaceId)".utf8),
                outputByteCount: 32
            )
            let aad = pairingAAD(
                offer: offer,
                phonePublicKey: grant.phonePublicKey,
                principal: principal
            )
            let plaintext = try open(envelope: grant.sealedWorkspace, key: wrapKey, aad: aad)
            let payload = try JSONDecoder().decode(PairingGrantPayload.self, from: plaintext)
            guard payload.workspaceId == offer.workspaceId,
                  payload.authorizedSubject == principal.subject,
                  payload.organizationId == principal.organizationId else {
                throw SyncCryptoError.principalMismatch
            }
            return try WorkspaceKey(base64URL: payload.workspaceKey)
        }
    }

    public static func seal(_ plaintext: Data, key: WorkspaceKey, aad: Data) throws -> SealedEnvelope {
        try seal(plaintext: plaintext, key: SymmetricKey(data: key.data), aad: aad)
    }

    public static func open(_ envelope: SealedEnvelope, key: WorkspaceKey, aad: Data) throws -> Data {
        try open(envelope: envelope, key: SymmetricKey(data: key.data), aad: aad)
    }

    /// Associated data is public but authenticated. Binding the envelope to a
    /// workspace, purpose, schema, and (for chunks) plaintext hash prevents a
    /// valid ciphertext from being replayed into a different context.
    public static func payloadAAD(workspaceId: String, schemaVersion: UInt16 = 1) -> Data {
        syncAAD(fields: ["clippy-sync-payload", String(schemaVersion), workspaceId])
    }

    public static func chunkAAD(workspaceId: String, hash: String, schemaVersion: UInt16 = 1) -> Data {
        syncAAD(fields: ["clippy-sync-chunk", String(schemaVersion), workspaceId, hash])
    }

    private static func seal(plaintext: Data, key: SymmetricKey, aad: Data) throws -> SealedEnvelope {
        let box = try ChaChaPoly.seal(plaintext, using: key, authenticating: aad)
        return SealedEnvelope(
            version: 1,
            nonce: Data(box.nonce).base64URLEncodedString(),
            ciphertext: (box.ciphertext + box.tag).base64URLEncodedString()
        )
    }

    private static func open(envelope: SealedEnvelope, key: SymmetricKey, aad: Data) throws -> Data {
        guard envelope.version == 1 else { throw SyncCryptoError.unsupportedVersion }
        let nonceData = try Data(base64URLEncoded: envelope.nonce)
        let combined = try Data(base64URLEncoded: envelope.ciphertext)
        guard combined.count >= 16 else { throw SyncCryptoError.invalidEncoding }
        let ciphertext = combined.dropLast(16)
        let tag = combined.suffix(16)
        do {
            let box = try ChaChaPoly.SealedBox(
                nonce: ChaChaPoly.Nonce(data: nonceData),
                ciphertext: ciphertext,
                tag: tag
            )
            return try ChaChaPoly.open(box, using: key, authenticating: aad)
        } catch {
            throw SyncCryptoError.authenticationFailed
        }
    }

    private static func pairingAAD(
        offer: PairingOffer,
        phonePublicKey: String,
        principal: AuthenticatedPrincipal
    ) -> Data {
        let fields = [
            String(offer.version), offer.workspaceId, offer.tunnelUrl,
            offer.workosIssuer, offer.workosAudience, String(offer.expiresAtMs), offer.macPublicKey,
            phonePublicKey, principal.subject, principal.organizationId ?? ""
        ]
        var result = Data()
        for field in fields {
            let bytes = Data(field.utf8)
            var length = UInt32(bytes.count).bigEndian
            withUnsafeBytes(of: &length) { result.append(contentsOf: $0) }
            result.append(bytes)
        }
        return result
    }

    private static func syncAAD(fields: [String]) -> Data {
        var result = Data()
        for field in fields {
            let bytes = Data(field.utf8)
            var length = UInt32(bytes.count).bigEndian
            withUnsafeBytes(of: &length) { result.append(contentsOf: $0) }
            result.append(bytes)
        }
        return result
    }

}

private struct PairingGrantPayload: Codable {
    let workspaceId: String
    let workspaceKey: String
    let authorizedSubject: String
    let organizationId: String?
}

public enum SyncCryptoError: Error, Equatable, Sendable {
    case invalidKey
    case invalidEncoding
    case unsupportedVersion
    case authenticationFailed
    case principalMismatch
}

extension Data {
    init(base64URLEncoded value: String) throws {
        var encoded = value.replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        encoded.append(String(repeating: "=", count: (4 - encoded.count % 4) % 4))
        guard let data = Data(base64Encoded: encoded) else {
            throw SyncCryptoError.invalidEncoding
        }
        self = data
    }

    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
