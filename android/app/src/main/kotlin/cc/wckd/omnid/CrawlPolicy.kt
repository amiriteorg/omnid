package cc.wckd.omnid

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.BatteryManager
import android.os.Build

/**
 * Crawls only on a charger and an unmetered network, unless the user allows more with
 * [KEY_ANYTIME]. Search keeps working either way; only fetching pauses. [apply] is called with
 * the new state whenever it changes, from any thread.
 */
internal class CrawlPolicy(
    private val context: Context,
    private val apply: (paused: Boolean, reason: String) -> Unit,
) {
    companion object {
        const val PREFS = "omnid"
        const val KEY_ANYTIME = "crawl_anytime"

        /** Why crawling should wait, as a string resource, or null to crawl. */
        fun pauseReason(anytime: Boolean, charging: Boolean, unmetered: Boolean): Int? = when {
            anytime || (charging && unmetered) -> null
            !charging && !unmetered -> R.string.paused_charger_wifi
            !charging -> R.string.paused_charger
            else -> R.string.paused_unmetered
        }
    }

    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    @Volatile private var charging = false
    @Volatile private var unmetered = false
    private var applied: Pair<Boolean, String>? = null

    private val battery = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            charging = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0
            update()
        }
    }

    private val network = object : ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            unmetered = capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
            update()
        }

        override fun onLost(network: Network) {
            unmetered = false
            update()
        }
    }

    private val anytime = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == KEY_ANYTIME) update()
    }

    fun start() {
        val filter = IntentFilter(Intent.ACTION_BATTERY_CHANGED)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(battery, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            context.registerReceiver(battery, filter)
        }
        connectivity.registerDefaultNetworkCallback(network)
        prefs.registerOnSharedPreferenceChangeListener(anytime)
        update()
    }

    fun stop() {
        context.unregisterReceiver(battery)
        connectivity.unregisterNetworkCallback(network)
        prefs.unregisterOnSharedPreferenceChangeListener(anytime)
    }

    @Synchronized
    private fun update() {
        val reason = pauseReason(prefs.getBoolean(KEY_ANYTIME, false), charging, unmetered)?.let(context::getString)
        val state = (reason != null) to reason.orEmpty()
        if (state != applied) {
            applied = state
            apply(state.first, state.second)
        }
    }
}
