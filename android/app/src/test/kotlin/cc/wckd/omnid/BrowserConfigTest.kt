package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BrowserConfigTest {
    @Test
    fun everyCriticalPrefIsSetAndFailsClosed() {
        for (name in BrowserConfig.critical) assertTrue(name, name in BrowserConfig.prefs)
        val prefs = BrowserConfig.prefs
        assertEquals(1, prefs["network.proxy.type"])
        assertEquals("127.0.0.1", prefs["network.proxy.socks"])
        assertTrue("a port no app can listen on", (prefs["network.proxy.socks_port"] as Int) < 1024)
        assertEquals(true, prefs["network.proxy.socks_remote_dns"])
        assertEquals(false, prefs["network.proxy.failover_direct"])
        assertEquals(true, prefs["network.dns.disabled"])
        assertEquals(false, prefs["media.peerconnection.enabled"])
    }

    @Test
    fun namesThePrefsGeckoDidNotTake() {
        val actual: Map<String, Any?> = BrowserConfig.prefs.toMutableMap().apply {
            put("network.proxy.type", 0)
            remove("network.dns.disabled")
        }
        assertEquals(listOf("network.proxy.type", "network.dns.disabled"), BrowserConfig.mismatched(actual))
        assertEquals(emptyList<String>(), BrowserConfig.mismatched(BrowserConfig.prefs))
    }

    @Test
    fun writesPrefsAsQuotedYaml() {
        val yaml = BrowserConfig.yaml(linkedMapOf("a.string" to "say \"hi\" \\ bye", "an.int" to 9, "a.bool" to false))
        assertEquals(
            "prefs:\n  \"a.string\": \"say \\\"hi\\\" \\\\ bye\"\n  \"an.int\": 9\n  \"a.bool\": false\n",
            yaml,
        )
    }

    @Test
    fun opensWebPagesAndKeepsEverythingElseOut() {
        assertEquals(BrowserConfig.Target.LOAD, BrowserConfig.target("https", "example.com"))
        assertEquals(BrowserConfig.Target.LOAD, BrowserConfig.target("HTTP", "x.onion"))
        assertEquals(BrowserConfig.Target.LOAD, BrowserConfig.target("about", null))
        assertEquals(BrowserConfig.Target.ELSEWHERE, BrowserConfig.target("magnet", null))
        for (scheme in listOf("file", "content", "intent", "tel", "mailto", "javascript", null)) {
            assertEquals("$scheme", BrowserConfig.Target.REFUSE, BrowserConfig.target(scheme, "x"))
        }
        assertFalse("no host", BrowserConfig.isWebPage("https", ""))
    }

    @Test
    fun turnsTypedTextIntoAddresses() {
        assertEquals("https://example.com/a", BrowserConfig.address(" example.com/a "))
        assertEquals("http://x.onion/", BrowserConfig.address("x.onion/"))
        assertEquals("http://stats.i2p", BrowserConfig.address("stats.i2p"))
        assertEquals("http://x.onion", BrowserConfig.address("http://x.onion"))
        assertEquals("https://example.com:8443", BrowserConfig.address("example.com:8443"))
        for (notAnAddress in listOf("", "tor browser", "localhost", "file:///etc/hosts", "javascript:alert(1)", ".onion")) {
            assertNull(notAnAddress, BrowserConfig.address(notAnAddress))
        }
    }
}
