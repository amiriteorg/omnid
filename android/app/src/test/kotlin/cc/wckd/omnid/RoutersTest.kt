package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class RoutersTest {
    @Test
    fun readsANodesAddress() {
        assertEquals("", HyphanetNode.parse("  "))
        assertEquals("http://100.101.102.103:8888/", HyphanetNode.parse(" 100.101.102.103 "))
        assertEquals("http://100.101.102.103:8888/", HyphanetNode.parse("http://100.101.102.103:8888"))
        assertEquals("http://home-server:9000/", HyphanetNode.parse("home-server:9000/"))
        assertEquals("https://node.example:443/", HyphanetNode.parse("HTTPS://node.example"))
        assertEquals("http://[fd7a:115c::1]:8888/", HyphanetNode.parse("http://[fd7a:115c::1]"))
    }

    @Test
    fun refusesWhatIsNotANode() {
        for (typed in listOf("ftp://x", "http://", "http://x/USK@abc/site/1/", "http://u:p@x", "http://x?a=b", "not a host", "http://[::1")) {
            assertNull(typed, HyphanetNode.parse(typed))
        }
    }

    @Test
    fun readsAnI2pRoutersAddress() {
        assertEquals("", I2pRouter.parse(""))
        assertEquals("http://100.74.26.84:4444/", I2pRouter.parse("100.74.26.84"))
        assertEquals("http://100.74.26.84:4444/", I2pRouter.parse(" http://100.74.26.84:4444/ "))
        assertEquals("http://mini:4445/", I2pRouter.parse("mini:4445"))
    }

    @Test
    fun refusesWhatIsNotAnI2pRouter() {
        // i2pd's proxy speaks plain HTTP only.
        for (typed in listOf("https://100.74.26.84:4444", "socks5://100.74.26.84:4447", "http://x/path", "two words")) {
            assertNull(typed, I2pRouter.parse(typed))
        }
    }
}
