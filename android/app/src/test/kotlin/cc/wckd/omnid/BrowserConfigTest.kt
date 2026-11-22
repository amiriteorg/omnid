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
        assertEquals(true, prefs["network.proxy.socks5_remote_dns"])
        assertTrue("checked before a page loads", "network.proxy.socks5_remote_dns" in BrowserConfig.critical)
        assertEquals(false, prefs["dom.serviceWorkers.enabled"])
        assertEquals(true, prefs["security.ssl.disable_session_identifiers"])
        assertEquals(false, prefs["network.proxy.failover_direct"])
        assertEquals(true, prefs["network.dns.disabled"])
        assertEquals(false, prefs["media.peerconnection.enabled"])
        // What a page could tell this phone apart by.
        assertEquals(2, prefs["privacy.spoof_english"])
        for (off in listOf("device.sensors.enabled", "dom.gamepad.enabled", "media.eme.enabled", "dom.webgpu.enabled")) {
            assertEquals(off, false, prefs[off])
        }
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
    fun retriesI2pSitesOverHttp() {
        for (url in listOf("https://stats.i2p/", "https://Forum.I2P./x?y", "https://abcd.b32.i2p:8443/")) {
            assertTrue(url, BrowserConfig.retriesOverHttp(url))
        }
        for (url in listOf("http://stats.i2p/", "https://example.com/", "https://x.onion/", "https://example.i2p.com/", "https://i2p/", "not a url", "https://[::1/", null)) {
            assertFalse("$url", BrowserConfig.retriesOverHttp(url))
        }
        assertTrue(BrowserConfig.isI2p("http://stats.i2p/"))
        assertFalse(BrowserConfig.isI2p("http://x.onion/"))
        assertTrue(BrowserConfig.retriesOverHttp("https://hyphanet/USK@abc,def,AQACAAE/site/1/"))
        assertTrue(BrowserConfig.isHyphanet("http://Hyphanet/CHK@x"))
        assertFalse(BrowserConfig.isHyphanet("http://hyphanet.example/"))
    }

    @Test
    fun showsAddressesShortAndWithoutTheScheme() {
        val onion = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion"
        assertEquals("2gzyxa5i…53wid.onion", BrowserConfig.displayAddress("http://$onion/"))
        assertEquals("www.bbcweb3h…ccrad.onion/news", BrowserConfig.displayAddress("https://www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion/news/"))
        assertEquals("example.com/a?b=c", BrowserConfig.displayAddress("https://example.com/a?b=c"))
        assertEquals("stats.i2p", BrowserConfig.displayAddress("http://stats.i2p"))
        assertEquals("7j74tcia…lswva.b32.i2p/x", BrowserConfig.displayAddress("http://7j74tciapju2shi3a337dl62zqcuqotha47y32ux4jbqgkzlswva.b32.i2p/x"))
        assertEquals(
            "hyphanet/USK@62H8KFSZ…/freenet-first-steps/24",
            BrowserConfig.displayAddress("http://hyphanet/USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps/24/"),
        )
        assertEquals("about:blank", BrowserConfig.displayAddress("about:blank"))
    }

    @Test
    fun turnsTypedTextIntoAddresses() {
        assertEquals("https://example.com/a", BrowserConfig.address(" example.com/a "))
        assertEquals("http://x.onion/", BrowserConfig.address("x.onion/"))
        assertEquals("http://stats.i2p", BrowserConfig.address("stats.i2p"))
        assertEquals("http://x.onion", BrowserConfig.address("http://x.onion"))
        assertEquals("https://example.com:8443", BrowserConfig.address("example.com:8443"))
        assertEquals("http://hyphanet/USK@abc,def,AQACAAE/site/1/", BrowserConfig.address("freenet:USK@abc,def,AQACAAE/site/1/"))
        assertEquals("http://hyphanet/CHK@abc,def,AAMC--8/file.pdf", BrowserConfig.address("CHK@abc,def,AAMC--8/file.pdf"))
        for (notAnAddress in listOf("", "tor browser", "localhost", "file:///etc/hosts", "javascript:alert(1)", ".onion")) {
            assertNull(notAnAddress, BrowserConfig.address(notAnAddress))
        }
    }

    @Test
    fun sharesHyphanetPagesByTheirKey() {
        assertEquals("freenet:USK@abc,def,AQACAAE/site/1/", BrowserConfig.shareable("http://hyphanet/USK@abc,def,AQACAAE/site/1/"))
        assertEquals("http://stats.i2p/", BrowserConfig.shareable("http://stats.i2p/"))
    }

    @Test
    fun entersAnOverlayOnlyFromAnotherNetwork() {
        val onion = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/"
        assertTrue(BrowserConfig.entersOverlay(onion, "http://stats.i2p/"))
        assertTrue(BrowserConfig.entersOverlay("https://example.com/", "http://hyphanet/USK@x/site/1/"))
        assertTrue(BrowserConfig.entersOverlay("http://stats.i2p/", "http://hyphanet/USK@x/site/1/"))
        assertFalse("within I2P", BrowserConfig.entersOverlay("http://stats.i2p/", "http://reg.i2p/"))
        assertFalse("to the clearnet", BrowserConfig.entersOverlay(onion, "https://example.com/"))
        assertFalse("a new tab, typed", BrowserConfig.entersOverlay(null, "http://stats.i2p/"))
    }

    @Test
    fun whatWasTypedStillLoadsThroughHttpsOnlyAndRedirects() {
        val page = "https://example.com/"
        val typed = "http://reg.i2p/"
        assertFalse("HTTPS-only's upgrade", BrowserConfig.refusedUnasked(page, typed, "https://reg.i2p/", redirect = true))
        assertFalse("and its reload over HTTP", BrowserConfig.refusedUnasked(page, "https://reg.i2p/", typed, redirect = false))
        assertFalse("a redirect within I2P", BrowserConfig.refusedUnasked(page, typed, "http://stats.i2p/", redirect = true))
        assertTrue("a script on the page", BrowserConfig.refusedUnasked(page, typed, "http://stats.i2p/", redirect = false))
        assertTrue("nothing was asked", BrowserConfig.refusedUnasked(page, null, "https://reg.i2p/", redirect = true))
        assertTrue("a redirect into Hyphanet", BrowserConfig.refusedUnasked(page, typed, "http://hyphanet/USK@x/site/1/", redirect = true))
        assertTrue("a page typed on the clearnet redirecting", BrowserConfig.refusedUnasked(null, page, "http://reg.i2p/", redirect = true))
        assertTrue(BrowserConfig.sameAddress("http://reg.i2p/a?b", "https://reg.i2p/a?b"))
        assertFalse(BrowserConfig.sameAddress("http://reg.i2p/a", "http://reg.i2p/b"))
        assertFalse(BrowserConfig.sameAddress("ftp://reg.i2p/", "http://reg.i2p/"))
        assertFalse(BrowserConfig.sameAddress(null, "http://reg.i2p/"))
    }
}
