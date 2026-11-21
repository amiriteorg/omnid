package cc.wckd.omnid

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import java.io.File
import java.security.SecureRandom
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Keeps the daemon running while the app is in the background: a foreground service, so the
 * index and routers stay up and crawling can continue on a charger.
 */
class OmnidService : Service() {
    /**
     * Where the running daemon listens, the token its API requires, and the port of the browser's
     * proxy, which takes the same token as its password.
     */
    data class Endpoint(val port: Int, val token: String, val browserProxyPort: Int) {
        val origin get() = "http://127.0.0.1:$port"
    }

    companion object {
        const val ACTION_STOP = "cc.wckd.omnid.STOP"
        private const val CHANNEL = "running"
        private const val NOTIFICATION_ID = 1

        @Volatile var endpoint: Endpoint? = null
            private set

        @Volatile var failure: String? = null
            private set

        private val listeners = CopyOnWriteArrayList<() -> Unit>()

        /** [listener] runs on a background thread whenever [endpoint] or [failure] changes. */
        fun observe(listener: () -> Unit) = listeners.add(listener)

        fun unobserve(listener: () -> Unit) = listeners.remove(listener)

        private fun changed() = listeners.forEach { it() }

        /**
         * Native calls block, so they run here, one at a time and in order: one thread for the
         * process, so a restart's start waits for the stop before it.
         */
        private val native: ExecutorService = Executors.newSingleThreadExecutor { Thread(it, "omnid-native") }
    }

    private var policy: CrawlPolicy? = null

    override fun onCreate() {
        super.onCreate()
        goForeground()
        // A fresh token per start: every app on the phone can reach loopback, only the WebView
        // gets this.
        val token = ByteArray(32).also(SecureRandom()::nextBytes).joinToString("") { "%02x".format(it) }
        native.execute {
            try {
                val node = getSharedPreferences(CrawlPolicy.PREFS, MODE_PRIVATE).getString(HyphanetNode.KEY, "").orEmpty()
                val port = Native.start(File(filesDir, "omnid").path, token, node)
                endpoint = Endpoint(port, token, Native.browserProxyPort())
                failure = null
                val crawlPolicy = CrawlPolicy(this) { paused, reason -> native.execute { Native.setPaused(paused, reason) } }
                policy = crawlPolicy
                mainExecutor.execute { crawlPolicy.start() }
            } catch (e: RuntimeException) {
                failure = e.message ?: e.toString()
            }
            changed()
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) stopSelf()
        // Not sticky: a restart from the background may not start a foreground service.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        policy?.stop()
        policy = null
        endpoint = null
        changed()
        native.execute { Native.stop() }
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun goForeground() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_name), NotificationManager.IMPORTANCE_LOW).apply {
                description = getString(R.string.channel_description)
            },
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 0, Intent(this, OmnidService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setColor(getColor(R.color.glow))
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, getString(R.string.stop), stop).build())
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }
}
