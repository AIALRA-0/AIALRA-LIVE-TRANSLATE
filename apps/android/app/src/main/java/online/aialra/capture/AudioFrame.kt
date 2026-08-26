package online.aialra.capture

import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Shared binary framing keeps Android retransmissions compatible with the Rust audio receiver. */
object AudioFrame {
    /** The 16-byte header contains the source sequence followed by capture time in big-endian order. */
    fun encode(sequence: Long, capturedAtMs: Long, pcmS16le: ByteArray): ByteArray {
        // The PCM payload stays little-endian while the transport identifiers stay network byte order.
        return ByteBuffer.allocate(16 + pcmS16le.size)
            .order(ByteOrder.BIG_ENDIAN)
            .putLong(sequence)
            .putLong(capturedAtMs)
            .put(pcmS16le)
            .array()
    }
}
