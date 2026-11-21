package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class CrawlPolicyTest {
    private fun reason(anytime: Boolean = false, charging: Boolean, unmetered: Boolean) =
        CrawlPolicy.pauseReason(anytime, charging, unmetered)

    @Test
    fun crawlsOnlyOnAChargerAndAnUnmeteredNetwork() {
        assertNull(reason(charging = true, unmetered = true))
        assertEquals(R.string.paused_charger_wifi, reason(charging = false, unmetered = false))
        assertEquals(R.string.paused_charger, reason(charging = false, unmetered = true))
        assertEquals(R.string.paused_unmetered, reason(charging = true, unmetered = false))
    }

    @Test
    fun crawlingAnytimeOverridesBoth() {
        for (charging in listOf(true, false)) {
            for (unmetered in listOf(true, false)) {
                assertNull(reason(anytime = true, charging = charging, unmetered = unmetered))
            }
        }
    }
}
