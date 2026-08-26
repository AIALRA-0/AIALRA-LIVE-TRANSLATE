package online.aialra.capture

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Transport tests prevent silent byte-order drift between phone and local server. */
class AudioFrameTest {
    @Test
    fun frameContainsBigEndianHeaderAndUnchangedPcm() {
        // The test decodes the same header fields read by the Rust WebSocket handler.
        val frame = AudioFrame.encode(42L, 1_234L, byteArrayOf(1, 2, 3, 4))
        val view = ByteBuffer.wrap(frame).order(ByteOrder.BIG_ENDIAN)
        assertEquals(42L, view.long)
        assertEquals(1_234L, view.long)
        val pcm = ByteArray(4)
        view.get(pcm)
        assertArrayEquals(byteArrayOf(1, 2, 3, 4), pcm)
    }
}
