import CryptoKit
import Foundation

public protocol DPoPProofSigning: Sendable {
    var jwkThumbprint: String { get }
    func proof(method: String, url: URL, accessToken: String?) throws -> String
}

public struct DPoPPublicJWK: Codable, Equatable, Sendable {
    public let crv: String
    public let kty: String
    public let x: String
    public let y: String

    fileprivate init(publicKey: P256.Signing.PublicKey) throws {
        let raw = publicKey.rawRepresentation
        let coordinateOffset: Int
        if raw.count == 64 { coordinateOffset = 0 }
        else if raw.count == 65, raw.first == 0x04 { coordinateOffset = 1 }
        else { throw DPoPError.invalidKey }
        let xStart = raw.index(raw.startIndex, offsetBy: coordinateOffset)
        let yStart = raw.index(xStart, offsetBy: 32)
        let coordinatesEnd = raw.index(yStart, offsetBy: 32)
        crv = "P-256"
        kty = "EC"
        x = Data(raw[xStart..<yStart]).dpopBase64URL
        y = Data(raw[yStart..<coordinatesEnd]).dpopBase64URL
    }

    fileprivate var rfc7638CanonicalJSON: Data {
        // RFC 7638 requires lexicographic member order and only the required
        // members. All values here are base64url/ASCII and need no escaping.
        Data("{\"crv\":\"\(crv)\",\"kty\":\"\(kty)\",\"x\":\"\(x)\",\"y\":\"\(y)\"}".utf8)
    }
}

/// A persistent-device-key DPoP proof signer. Callers must supply an already
/// environment-scoped Keychain account to `loadOrCreate`.
public struct DPoPSigner: DPoPProofSigning, Sendable {
    private let privateKey: P256.Signing.PrivateKey
    public let publicJWK: DPoPPublicJWK
    public let jwkThumbprint: String

    public static func loadOrCreate(
        keychain: KeychainStore = KeychainStore(),
        account: String
    ) throws -> DPoPSigner {
        guard !account.isEmpty else { throw DPoPError.invalidAccount }
        if let stored = try keychain.load(account: account) {
            do {
                return try DPoPSigner(
                    privateKey: P256.Signing.PrivateKey(rawRepresentation: stored)
                )
            } catch {
                // Never silently rotate a malformed persisted key: doing so
                // changes its JWK thumbprint and breaks the token binding.
                throw DPoPError.invalidStoredKey
            }
        }

        let signer = try DPoPSigner(privateKey: P256.Signing.PrivateKey())
        try keychain.save(signer.privateKey.rawRepresentation, account: account)
        return signer
    }

    init(privateKey: P256.Signing.PrivateKey) throws {
        self.privateKey = privateKey
        let jwk = try DPoPPublicJWK(publicKey: privateKey.publicKey)
        publicJWK = jwk
        jwkThumbprint = Data(SHA256.hash(data: jwk.rfc7638CanonicalJSON)).dpopBase64URL
    }

    public func proof(method: String, url: URL, accessToken: String?) throws -> String {
        try proof(
            method: method,
            url: url,
            accessToken: accessToken,
            issuedAt: Date(),
            jti: UUID()
        )
    }

    /// Creates an ES256 DPoP proof with injectable time/identifier for tests.
    /// `htu` retains the request URI but always removes its query and fragment,
    /// and `ath` is present only when an access token is supplied.
    public func proof(
        method: String,
        url: URL,
        accessToken: String?,
        issuedAt: Date,
        jti: UUID
    ) throws -> String {
        let htm = try Self.normalizedMethod(method)
        let htu = try Self.proofURL(url)
        let seconds = issuedAt.timeIntervalSince1970.rounded(.down)
        guard seconds.isFinite,
              seconds >= Double(Int64.min), seconds <= Double(Int64.max) else {
            throw DPoPError.invalidIssuedAt
        }

        let header = Header(typ: "dpop+jwt", alg: "ES256", jwk: publicJWK)
        let claims = Claims(
            htm: htm,
            htu: htu,
            iat: Int64(seconds),
            jti: jti.uuidString.lowercased(),
            ath: accessToken.map { token in
                Data(SHA256.hash(data: Data(token.utf8))).dpopBase64URL
            }
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let headerSegment = try encoder.encode(header).dpopBase64URL
        let claimSegment = try encoder.encode(claims).dpopBase64URL
        let signingInput = "\(headerSegment).\(claimSegment)"
        let signature = try privateKey.signature(for: Data(signingInput.utf8)).rawRepresentation
        guard signature.count == 64 else { throw DPoPError.invalidSignature }
        return "\(signingInput).\(signature.dpopBase64URL)"
    }

    private static func normalizedMethod(_ method: String) throws -> String {
        let bytes = Array(method.utf8)
        guard !bytes.isEmpty, bytes.count <= 32,
              bytes.allSatisfy(Self.isHTTPTokenByte) else {
            throw DPoPError.invalidMethod
        }
        return method.uppercased()
    }

    private static func isHTTPTokenByte(_ byte: UInt8) -> Bool {
        (48...57).contains(byte) || (65...90).contains(byte) ||
            (97...122).contains(byte) ||
            [33, 35, 36, 37, 38, 39, 42, 43, 45, 46, 94, 95, 96, 124, 126].contains(byte)
    }

    private static func proofURL(_ url: URL) throws -> String {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let scheme = components.scheme?.lowercased(),
              ["http", "https"].contains(scheme),
              components.host != nil,
              components.user == nil,
              components.password == nil else {
            throw DPoPError.invalidURL
        }
        components.query = nil
        components.fragment = nil
        guard let stripped = components.url else { throw DPoPError.invalidURL }
        return stripped.absoluteString
    }
}

public enum DPoPError: Error, Equatable, Sendable {
    case invalidAccount
    case invalidStoredKey
    case invalidKey
    case invalidMethod
    case invalidURL
    case invalidIssuedAt
    case invalidSignature
}

private struct Header: Encodable {
    let typ: String
    let alg: String
    let jwk: DPoPPublicJWK
}

private struct Claims: Encodable {
    let htm: String
    let htu: String
    let iat: Int64
    let jti: String
    let ath: String?
}

private extension Data {
    var dpopBase64URL: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
