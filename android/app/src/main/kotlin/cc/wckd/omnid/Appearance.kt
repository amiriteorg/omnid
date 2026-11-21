package cc.wckd.omnid

import android.app.Activity
import android.content.Context
import android.content.res.Configuration

/**
 * Settings → Theme on OMNId's own screens. Each activity calls [apply] from attachBaseContext, so
 * its resources (the night colors, the search page's prefers-color-scheme) resolve the chosen way
 * before anything is drawn; System leaves the phone's setting alone.
 */
internal object Appearance {
    fun apply(activity: Activity, base: Context) {
        val night = when (chosen(base)) {
            Settings.Theme.SYSTEM -> return
            Settings.Theme.LIGHT -> Configuration.UI_MODE_NIGHT_NO
            Settings.Theme.DARK -> Configuration.UI_MODE_NIGHT_YES
        }
        // Only the night bits: the rest of the configuration stays the system's.
        activity.applyOverrideConfiguration(Configuration().apply { uiMode = night })
    }

    /**
     * Settings → Allow screenshots on [activity]'s window: off (the default), screenshots and the
     * recent-apps preview show nothing of it. Every screen of OMNId shows something private: the
     * pages, the searches, the proxy's password.
     */
    fun applyScreenshots(activity: Activity) {
        val allowed = activity.getSharedPreferences(CrawlPolicy.PREFS, Context.MODE_PRIVATE).getBoolean(Settings.SCREENSHOTS, false)
        if (allowed) {
            activity.window.clearFlags(android.view.WindowManager.LayoutParams.FLAG_SECURE)
        } else {
            activity.window.addFlags(android.view.WindowManager.LayoutParams.FLAG_SECURE)
        }
    }

    /** The theme setting now, to compare with the one a screen was drawn with. */
    fun chosen(context: Context): Settings.Theme =
        Settings.theme(context.getSharedPreferences(CrawlPolicy.PREFS, Context.MODE_PRIVATE).getString(Settings.THEME, null))
}
