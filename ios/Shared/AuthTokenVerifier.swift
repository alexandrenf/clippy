import Foundation
import Security

public struct VerifiedJWT: Equatable, Sendable {
    public let subject: String
    public let organizationId: String?
    public let expiresAt: Date

    public init(subject: String, organizationId: String?, expiresAt: Date) {
        self.subject = subject
        self.organizationId = organizationId
        self.expiresAt = expiresAt
    }
}

public enum JWTTokenKind: Sendable {
    case access
    case id
}

public protocol JWKSLoading: Sendable {
    func loadJWKS(from url: URL) async throws -> Data
}

public struct URLSessionJWKSLoader: JWKSLoading, Sendable {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func loadJWKS(from url: URL) async throws -> Data {
        var request = URLRequest(url: url)
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = 15
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
        let (data, response) = try await session.data(for: request)
        guard let response = response as? HTTPURLResponse,
              response.statusCode == 200,
              data.count <= 1_048_576 else {
            throw JWTVerificationError.invalidJWKSResponse
        }
        return data
    }
}

/// Verifies WorkOS JWTs locally before any token is persisted or used. The
/// verifier accepts RS256 only and converts RSA JWK modulus/exponent values to
/// the PKCS#1 public-key representation expected by Security.framework.
public actor JWTVerifier {
    private let issuer: URL
    private let audience: String
    private let loader: any JWKSLoading
    private let now: @Sendable () -> Date
    private let cacheLifetime: TimeInterval
    private var cachedJWKS: JWKS?
    private var cacheExpiresAt: Date = .distantPast

    public init(
        issuer: URL,
        audience: String,
        session: URLSession = .shared,
        cacheLifetime: TimeInterval = 3_600,
        now: @escaping @Sendable () -> Date = { Date() }
    ) throws {
        try self.init(
            issuer: issuer,
            audience: audience,
            loader: URLSessionJWKSLoader(session: session),
            cacheLifetime: cacheLifetime,
            now: now
        )
    }

    public init(
        issuer: URL,
        audience: String,
        loader: any JWKSLoading,
        cacheLifetime: TimeInterval = 3_600,
        now: @escaping @Sendable () -> Date = { Date() }
    ) throws {
        guard issuer.scheme == "https", issuer.host != nil,
              issuer.user == nil, issuer.password == nil,
              !audience.isEmpty, cacheLifetime >= 0 else {
            throw JWTVerificationError.invalidConfiguration
        }
        self.issuer = issuer
        self.audience = audience
        self.loader = loader
        self.cacheLifetime = cacheLifetime
        self.now = now
    }

    public func verify(
        _ token: String,
        kind: JWTTokenKind = .access,
        expectedNonce: String? = nil
    ) async throws -> VerifiedJWT {
        guard token.utf8.count <= 65_536 else { throw JWTVerificationError.invalidToken }
        let segments = token.split(separator: ".", omittingEmptySubsequences: false)
        guard segments.count == 3,
              segments.allSatisfy({ String($0).isBase64URLSegment }) else {
            throw JWTVerificationError.invalidToken
        }
        let header = try decode(JWTHeader.self, segment: segments[0])
        guard header.alg == "RS256", let kid = header.kid, !kid.isEmpty, kid.utf8.count <= 512 else {
            throw JWTVerificationError.unsupportedAlgorithm
        }

        var keys = try await loadKeys(forceRefresh: false)
        var jwk = keys.keys.first { $0.kid == kid }
        if jwk == nil {
            keys = try await loadKeys(forceRefresh: true)
            jwk = keys.keys.first { $0.kid == kid }
        }
        guard let jwk,
              jwk.kty == "RSA",
              jwk.use == nil || jwk.use == "sig",
              jwk.alg == nil || jwk.alg == "RS256" else {
            throw JWTVerificationError.keyNotFound
        }
        let publicKey = try RSAKeyDER.publicKey(modulus: jwk.n, exponent: jwk.e)
        let signingInput = Data("\(segments[0]).\(segments[1])".utf8)
        let signature: Data
        do { signature = try Data(base64URLEncoded: String(segments[2])) }
        catch { throw JWTVerificationError.invalidToken }
        let algorithm: SecKeyAlgorithm = .rsaSignatureMessagePKCS1v15SHA256
        guard SecKeyIsAlgorithmSupported(publicKey, .verify, algorithm) else {
            throw JWTVerificationError.unsupportedAlgorithm
        }
        var verificationError: Unmanaged<CFError>?
        guard SecKeyVerifySignature(
            publicKey,
            algorithm,
            signingInput as CFData,
            signature as CFData,
            &verificationError
        ) else {
            _ = verificationError?.takeRetainedValue()
            throw JWTVerificationError.invalidSignature
        }

        let claims = try decode(JWTClaims.self, segment: segments[1])
        guard claims.iss == issuer.absoluteString else {
            throw JWTVerificationError.wrongIssuer
        }
        switch kind {
        case .access:
            guard claims.aud?.values.contains(audience) == true else {
                throw JWTVerificationError.wrongAudience
            }
        case .id:
            guard claims.aud?.values.contains(audience) == true else {
                throw JWTVerificationError.wrongAudience
            }
        }
        guard !claims.sub.isEmpty, claims.sub.utf8.count <= 1_024 else {
            throw JWTVerificationError.invalidSubject
        }
        guard claims.exp.isFinite else { throw JWTVerificationError.expired }
        let expiration = Date(timeIntervalSince1970: claims.exp)
        guard expiration > now() else {
            throw JWTVerificationError.expired
        }
        if let expectedNonce {
            guard let nonce = claims.nonce, Self.secureEquals(nonce, expectedNonce) else {
                throw JWTVerificationError.wrongNonce
            }
        }
        return VerifiedJWT(
            subject: claims.sub,
            organizationId: claims.orgId,
            expiresAt: expiration
        )
    }

    private func loadKeys(forceRefresh: Bool) async throws -> JWKS {
        if !forceRefresh, let cachedJWKS, now() < cacheExpiresAt { return cachedJWKS }
        let endpoint = issuer.appending(path: "oauth2/jwks")
        let data: Data
        do { data = try await loader.loadJWKS(from: endpoint) }
        catch { throw JWTVerificationError.invalidJWKSResponse }
        let keys: JWKS
        do { keys = try JSONDecoder().decode(JWKS.self, from: data) }
        catch { throw JWTVerificationError.invalidJWKSResponse }
        guard !keys.keys.isEmpty, keys.keys.count <= 64 else {
            throw JWTVerificationError.invalidJWKSResponse
        }
        cachedJWKS = keys
        cacheExpiresAt = now().addingTimeInterval(cacheLifetime)
        return keys
    }

    private func decode<T: Decodable>(_ type: T.Type, segment: Substring) throws -> T {
        do {
            let data = try Data(base64URLEncoded: String(segment))
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw JWTVerificationError.invalidToken
        }
    }

    private static func secureEquals(_ left: String, _ right: String) -> Bool {
        let left = Array(left.utf8)
        let right = Array(right.utf8)
        guard left.count == right.count else { return false }
        return zip(left, right).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
    }
}

public enum JWTVerificationError: Error, Equatable, Sendable {
    case invalidConfiguration
    case invalidToken
    case unsupportedAlgorithm
    case invalidJWKSResponse
    case keyNotFound
    case invalidKey
    case invalidSignature
    case wrongIssuer
    case wrongAudience
    case invalidSubject
    case expired
    case wrongNonce
}

private struct JWTHeader: Decodable, Sendable {
    let alg: String
    let kid: String?
}

private struct JWTClaims: Decodable, Sendable {
    let iss: String
    let aud: JWTAudience?
    let exp: TimeInterval
    let sub: String
    let nonce: String?
    let orgId: String?

    enum CodingKeys: String, CodingKey {
        case iss, aud, exp, sub, nonce
        case orgId = "org_id"
    }
}

private struct JWTAudience: Decodable, Sendable {
    let values: [String]

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let value = try? container.decode(String.self) { values = [value] }
        else { values = try container.decode([String].self) }
    }
}

private struct JWKS: Decodable, Sendable {
    let keys: [RSAJWK]
}

private struct RSAJWK: Decodable, Sendable {
    let kty: String
    let use: String?
    let alg: String?
    let kid: String
    let n: String
    let e: String
}

enum RSAKeyDER {
    static func publicKey(modulus: String, exponent: String) throws -> SecKey {
        let modulusData: Data
        let exponentData: Data
        do {
            modulusData = try Data(base64URLEncoded: modulus)
            exponentData = try Data(base64URLEncoded: exponent)
        } catch {
            throw JWTVerificationError.invalidKey
        }
        guard modulusData.count >= 256, modulusData.count <= 1_024,
              !exponentData.isEmpty, exponentData.count <= 8,
              modulusData.contains(where: { $0 != 0 }),
              exponentData.contains(where: { $0 != 0 }) else {
            throw JWTVerificationError.invalidKey
        }
        let encoded = sequence(integer(modulusData) + integer(exponentData))
        let attributes: [String: Any] = [
            kSecAttrKeyType as String: kSecAttrKeyTypeRSA,
            kSecAttrKeyClass as String: kSecAttrKeyClassPublic,
            kSecAttrKeySizeInBits as String: modulusData.count * 8
        ]
        var error: Unmanaged<CFError>?
        guard let key = SecKeyCreateWithData(
            encoded as CFData,
            attributes as CFDictionary,
            &error
        ) else {
            _ = error?.takeRetainedValue()
            throw JWTVerificationError.invalidKey
        }
        return key
    }

    /// Test support for producing a JWK from Security.framework's PKCS#1 RSA
    /// public-key external representation.
    static func jwkComponents(pkcs1PublicKey: Data) throws -> (n: String, e: String) {
        var outer = DERReader(data: pkcs1PublicKey)
        let sequence = try outer.read(tag: 0x30)
        guard outer.isAtEnd else { throw JWTVerificationError.invalidKey }
        var values = DERReader(data: sequence)
        let modulus = try values.read(tag: 0x02).droppingIntegerPadding
        let exponent = try values.read(tag: 0x02).droppingIntegerPadding
        guard values.isAtEnd, !modulus.isEmpty, !exponent.isEmpty else {
            throw JWTVerificationError.invalidKey
        }
        return (modulus.base64URLEncodedString(), exponent.base64URLEncodedString())
    }

    private static func sequence(_ value: Data) -> Data { tlv(tag: 0x30, value: value) }

    private static func integer(_ input: Data) -> Data {
        var value = input.droppingIntegerPadding
        if value.isEmpty { value = Data([0]) }
        if let first = value.first, first & 0x80 != 0 { value.insert(0, at: 0) }
        return tlv(tag: 0x02, value: value)
    }

    private static func tlv(tag: UInt8, value: Data) -> Data {
        Data([tag]) + encodedLength(value.count) + value
    }

    private static func encodedLength(_ length: Int) -> Data {
        if length < 128 { return Data([UInt8(length)]) }
        var value = length
        var bytes: [UInt8] = []
        while value > 0 {
            bytes.insert(UInt8(value & 0xff), at: 0)
            value >>= 8
        }
        return Data([0x80 | UInt8(bytes.count)] + bytes)
    }
}

private struct DERReader {
    let data: Data
    private(set) var index = 0
    var isAtEnd: Bool { index == data.count }

    mutating func read(tag expectedTag: UInt8) throws -> Data {
        guard index < data.count, data[index] == expectedTag else {
            throw JWTVerificationError.invalidKey
        }
        index += 1
        let length = try readLength()
        guard length >= 0, index <= data.count, length <= data.count - index else {
            throw JWTVerificationError.invalidKey
        }
        defer { index += length }
        return Data(data[index..<(index + length)])
    }

    private mutating func readLength() throws -> Int {
        guard index < data.count else { throw JWTVerificationError.invalidKey }
        let first = data[index]
        index += 1
        if first & 0x80 == 0 { return Int(first) }
        let count = Int(first & 0x7f)
        guard count > 0, count <= MemoryLayout<Int>.size,
              index <= data.count, count <= data.count - index else {
            throw JWTVerificationError.invalidKey
        }
        var length = 0
        for _ in 0..<count {
            guard length <= (Int.max >> 8) else { throw JWTVerificationError.invalidKey }
            length = (length << 8) | Int(data[index])
            index += 1
        }
        return length
    }
}

private extension Data {
    var droppingIntegerPadding: Data {
        var result = self
        while result.count > 1, result.first == 0 { result.removeFirst() }
        return result
    }
}

private extension String {
    var isBase64URLSegment: Bool {
        !isEmpty && utf8.allSatisfy { byte in
            (48...57).contains(byte) || (65...90).contains(byte) ||
                (97...122).contains(byte) || byte == 45 || byte == 95
        }
    }
}
