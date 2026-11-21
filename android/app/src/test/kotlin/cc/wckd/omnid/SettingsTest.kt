package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SettingsTest {
    @Test
    fun readsStoredChoicesOrTheirDefaults() {
        assertEquals(Settings.Theme.DARK, Settings.theme("DARK"))
        assertEquals(Settings.Theme.SYSTEM, Settings.theme(null))
        assertEquals(Settings.Theme.SYSTEM, Settings.theme("sepia"))
        assertEquals(Settings.CrawlSpeed.FAST, Settings.crawlSpeed("FAST"))
        assertEquals(Settings.CrawlSpeed.NORMAL, Settings.crawlSpeed(null))
        // The daemon reads the speed by its position (omnid-android's CrawlSpeed::from_ordinal).
        assertEquals(listOf(0, 1, 2), Settings.CrawlSpeed.entries.map { it.ordinal })
        assertEquals(1.3f, Settings.textSize(1.3f))
        assertEquals(1f, Settings.textSize(7f))
    }

    @Test
    fun searchesEveryNetworkUnlessSomeWereChosen() {
        assertEquals(Settings.NETWORKS.toSet(), Settings.networks(null))
        assertEquals(Settings.NETWORKS.toSet(), Settings.networks(""))
        assertEquals("never none", Settings.NETWORKS.toSet(), Settings.networks("gopher"))
        assertEquals(setOf("onion", "i2p"), Settings.networks("i2p, onion,gopher"))
        assertEquals("onion,i2p", Settings.storeNetworks(setOf("i2p", "onion")))
    }

    @Test
    fun readsLanguagesAsPrimaryTags() {
        assertEquals("", Settings.languages("  "))
        assertEquals("de,en", Settings.languages("de, EN-gb"))
        assertEquals("de,en", Settings.languages("de en de_AT"))
        assertEquals("fil", Settings.languages("fil"))
        for (typed in listOf("english", "d", "de, 12", "a,b,c,d,e,f".replace(Regex("[a-f]")) { it.value.repeat(2) })) {
            assertNull(typed, Settings.languages(typed))
        }
    }

    @Test
    fun cleansListsAsTyped() {
        assertEquals(listOf("drugs", "two words"), Settings.entries(" drugs \n\n# a comment\ntwo words\ndrugs\n"))
        assertEquals(1_000, Settings.entries((1..2_000).joinToString("\n") { "w$it" }).size)
        assertEquals(emptyList<String>(), Settings.entries("x".repeat(201)))
    }

    @Test
    fun buildsTheSearchPagesAddress() {
        val all = Settings.NETWORKS.toSet()
        assertEquals("http://127.0.0.1:7171/?embedded", Settings.searchPage("http://127.0.0.1:7171", all, ""))
        assertEquals(
            "http://127.0.0.1:7171/?embedded&networks=onion,i2p&lang=de,en&q=tor%20browser%20%26%20more",
            Settings.searchPage("http://127.0.0.1:7171", setOf("i2p", "onion"), "de,en", " tor browser & more "),
        )
    }

    @Test
    fun blocksImagesWithAPrefAndKeepsTheLevels() {
        val prefs = Settings.geckoPrefs(SecurityLevel.SAFEST, images = false)
        assertEquals(2, prefs["permissions.default.image"])
        assertEquals(true, prefs["svg.disabled"])
        assertEquals(1, Settings.geckoPrefs(SecurityLevel.STANDARD, images = true)["permissions.default.image"])
    }

    @Test
    fun makesTypeablePasswords() {
        assertEquals("aaaa-aaaa-aaaa-aaaa-aaaa-aaaa", Settings.password(ByteArray(15)))
        assertEquals("7777-7777-7777-7777-7777-7777", Settings.password(ByteArray(15) { -1 }))
        val password = Settings.password(ByteArray(15) { it.toByte() })
        assertEquals(29, password.length)
        assertEquals(true, password.all { it in 'a'..'z' || it in '2'..'7' || it == '-' })
    }
}
