import CryptoKit
import Foundation

public struct ChunkDescriptor: Codable, Equatable, Sendable {
    public let sha256: String
    public let size: UInt64
}

public struct FileManifest: Codable, Equatable, Sendable {
    public let schemaVersion: UInt8
    public let fileSha256: String
    public let size: UInt64
    public let chunkSize: UInt32
    public let chunks: [ChunkDescriptor]

    public static func make(data: Data, chunkSize: Int = 1_048_576) throws -> FileManifest {
        guard chunkSize > 0, chunkSize <= Int(UInt32.max) else {
            throw FileManifestError.invalidChunkSize
        }
        var chunks: [ChunkDescriptor] = []
        var offset = 0
        while offset < data.count {
            let end = min(offset + chunkSize, data.count)
            let chunk = data[offset..<end]
            chunks.append(ChunkDescriptor(sha256: chunk.sha256Hex, size: UInt64(chunk.count)))
            offset = end
        }
        return FileManifest(
            schemaVersion: 1,
            fileSha256: data.sha256Hex,
            size: UInt64(data.count),
            chunkSize: UInt32(chunkSize),
            chunks: chunks
        )
    }

    public func verify(reconstructed data: Data) -> Bool {
        UInt64(data.count) == size && data.sha256Hex == fileSha256
    }
}

public enum FileManifestError: Error { case invalidChunkSize }

private extension DataProtocol {
    var sha256Hex: String {
        SHA256.hash(data: self).map { String(format: "%02x", $0) }.joined()
    }
}
