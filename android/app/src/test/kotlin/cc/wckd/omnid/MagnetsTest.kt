package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MagnetsTest {
    private val hash = "08ada5a7a6183aae1e09d831df6748d566095a10"

    @Test
    fun keepsTheHashAndNameOnly() {
        val planted = "magnet:?xt=urn:btih:$hash&dn=Sintel%20movie&tr=udp%3A%2F%2Ftracker.attacker%3A6969" +
            "&x.pe=203.0.113.9:6881&ws=http://attacker/seed&as=http://attacker/a&xs=http://attacker/x"
        assertEquals("magnet:?xt=urn:btih:$hash&dn=Sintel%20movie", Magnets.safe(planted))
        assertEquals("magnet:?xt=urn:btih:$hash", Magnets.safe("MAGNET:?tr=udp://t:1&xt=urn:btih:$hash"))
        val v2 = "urn:btmh:1220" + "a".repeat(64)
        assertEquals("magnet:?xt=$v2", Magnets.safe("magnet:?xt=$v2"))
        assertEquals("magnet:?xt=urn:btih:${"A".repeat(32)}", Magnets.safe("magnet:?xt=urn:btih:${"A".repeat(32)}"))
    }

    @Test
    fun refusesLinksWithoutAHash() {
        for (link in listOf("magnet:?dn=x", "magnet:?xt=urn:btih:123", "magnet:?xt=urn:sha1:$hash", "http://x/?xt=urn:btih:$hash", "magnet:?xt=%zz")) {
            assertNull(link, Magnets.safe(link))
        }
    }
}
