package cc.wckd.omnid

import android.content.Context
import android.content.Intent

/**
 * The i2pd app (PurpleI2P, on F-Droid), which runs an I2P router on the phone. Its HTTP proxy
 * listens on 127.0.0.1:4444, where the daemon looks by default, so omnid reaches `.i2p` sites as
 * soon as the router runs, with nothing to set up. The manifest names the package under
 * `<queries>`: without that, Android 11+ hides whether it is installed.
 */
internal object I2pd {
    private const val PACKAGE = "org.purplei2p.i2pd"
    private const val LISTING = "https://f-droid.org/packages/$PACKAGE/"

    fun isInstalled(context: Context): Boolean = context.packageManager.getLaunchIntentForPackage(PACKAGE) != null

    /**
     * i2pd, where its router starts, or its F-Droid page when it is not installed, in OMNId's own
     * browser: the phone's would fetch it directly.
     */
    fun intent(context: Context): Intent =
        context.packageManager.getLaunchIntentForPackage(PACKAGE)
            ?: Intent(context, BrowserActivity::class.java).putExtra(BrowserActivity.EXTRA_URL, LISTING)
}
