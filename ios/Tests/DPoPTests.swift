import CryptoKit
import Foundation
import Testing
@testable import ClippySyncCore

@Test func dpopProofHasCanonicalHeaderClaimsAndVerifiableRawSignature() throws {
    let privateKey = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
    let signer = try DPoPSigner(privateKey: privateKey)
    let issuedAt = Date(timeIntervalSince1970: 1_786_276_800.875)
    let jti = UUID(uuidString: "11111111-1111-4111-8111-111111111111")!
    let proof = try signer.proof(
        method: "post",
        url: URL(string: "https://sync.example.com/v1/exchange?cursor=secret#section")!,
        accessToken: "access-token",
        issuedAt: issuedAt,
        jti: jti
    )
    let segments = proof.split(separator: ".").map(String.init)
    #expect(segments.count == 3)

    let header = try decodedJSONObject(segment: segments[0])
    #expect(header["typ"] as? String == "dpop+jwt")
    #expect(header["alg"] as? String == "ES256")
    let jwk = try #require(header["jwk"] as? [String: Any])
    #expect(jwk["kty"] as? String == "EC")
    #expect(jwk["crv"] as? String == "P-256")
    #expect(jwk["x"] as? String == signer.publicJWK.x)
    #expect(jwk["y"] as? String == signer.publicJWK.y)

    let claims = try decodedJSONObject(segment: segments[1])
    #expect(claims["htm"] as? String == "POST")
    #expect(claims["htu"] as? String == "https://sync.example.com/v1/exchange")
    #expect(claims["iat"] as? Int64 == 1_786_276_800)
    #expect(claims["jti"] as? String == jti.uuidString.lowercased())
    #expect(
        claims["ath"] as? String ==
            Data(SHA256.hash(data: Data("access-token".utf8))).testBase64URL
    )

    let signatureData = try decodeBase64URL(segments[2])
    #expect(signatureData.count == 64)
    let signature = try P256.Signing.ECDSASignature(rawRepresentation: signatureData)
    #expect(privateKey.publicKey.isValidSignature(
        signature,
        for: Data("\(segments[0]).\(segments[1])".utf8)
    ))
}

@Test func dpopThumbprintUsesRFC7638CanonicalMembersAndOrdering() throws {
    let privateKey = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
    let signer = try DPoPSigner(privateKey: privateKey)
    let canonical = "{\"crv\":\"P-256\",\"kty\":\"EC\",\"x\":\"\(signer.publicJWK.x)\",\"y\":\"\(signer.publicJWK.y)\"}"

    #expect(
        signer.jwkThumbprint ==
            Data(SHA256.hash(data: Data(canonical.utf8))).testBase64URL
    )
    #expect(signer.jwkThumbprint == "Nrqg3-M_Xwtx-1tbtc1J7Xul2DyeC0bUSy9u_5NSG6g")
}

@Test func dpopProofOmitsAthAndStripsQueryAndFragment() throws {
    let privateKey = try P256.Signing.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
    let signer = try DPoPSigner(privateKey: privateKey)
    let proof = try signer.proof(
        method: "GET",
        url: URL(string: "https://example.com:8443/a%20path?token=private#fragment")!,
        accessToken: nil,
        issuedAt: Date(timeIntervalSince1970: 1_700_000_000),
        jti: UUID(uuidString: "22222222-2222-4222-8222-222222222222")!
    )
    let payload = try decodedJSONObject(segment: proof.split(separator: ".").map(String.init)[1])

    #expect(payload["htu"] as? String == "https://example.com:8443/a%20path")
    #expect(payload["ath"] == nil)
}

private func decodedJSONObject(segment: String) throws -> [String: Any] {
    try #require(
        JSONSerialization.jsonObject(with: decodeBase64URL(segment)) as? [String: Any]
    )
}

private func decodeBase64URL(_ value: String) throws -> Data {
    var encoded = value
        .replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    encoded.append(String(repeating: "=", count: (4 - encoded.count % 4) % 4))
    guard let data = Data(base64Encoded: encoded) else { throw DPoPTestError.invalidBase64 }
    return data
}

private extension Data {
    var testBase64URL: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

private enum DPoPTestError: Error { case invalidBase64 }
