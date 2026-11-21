package cc.wckd.omnid

import android.app.DownloadManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.MediaStore
import android.text.format.Formatter
import java.io.IOException
import java.io.InputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import org.mozilla.geckoview.WebResponse

/**
 * Files the browser hands over, saved to the phone's Downloads. When the server serves byte ranges
 * and the file is large, the daemon fetches it in parallel ranges, each connection on a Tor circuit
 * of its own (`Native.download`); otherwise, or when the daemon's answer is not the file the
 * browser saw, it comes over the browser's own connection. Either way nothing leaves except through
 * the daemon. A notification shows progress and offers Cancel; OMNId never opens a file itself.
 */
internal object Downloader {
    const val ACTION_CANCEL = "cc.wckd.omnid.CANCEL_DOWNLOAD"
    const val EXTRA_ID = "download"
    private const val CHANNEL = "downloads"
    private const val POLL_MS = 500L

    /** The daemon's states for a download (`omnid-android`'s `DownloadState`). */
    private const val PROBING = 0L
    private const val RUNNING = 1L
    private const val DONE = 2L
    private const val FAILED = 3L
    private const val CHANGED = 4L
    private const val CANCELLED = 5L

    /** Notification ids, apart from the service's. */
    private val ids = AtomicInteger(1_000)
    private val jobs = ConcurrentHashMap<Int, Job>()

    private class Job(val id: Int, val name: String) {
        @Volatile var cancelled = false
    }

    private class Cancelled : IOException("cancelled")

    /**
     * Saves [response]'s file, found on a page of [site], asking for it with [userAgent] (the
     * browser's, so the server sees one client). Call on the main thread; the work runs on a
     * thread of its own.
     */
    fun start(context: Context, response: WebResponse, site: String, userAgent: String, connections: Int) {
        val app = context.applicationContext
        val headers = response.headers
        val mime = Downloads.header(headers, "Content-Type")?.substringBefore(';')?.trim()?.lowercase()
            ?.takeIf { it.isNotEmpty() } ?: "application/octet-stream"
        val name = Downloads.fileName(Downloads.header(headers, "Content-Disposition"), response.uri, mime)
        val job = Job(ids.incrementAndGet(), name)
        jobs[job.id] = job
        Thread({ run(app, job, response, site, userAgent, connections, mime) }, "omnid-download-${job.id}").start()
    }

    /** From the notification's Cancel. */
    fun cancel(id: Int) {
        jobs[id]?.cancelled = true
    }

    private fun run(context: Context, job: Job, response: WebResponse, site: String, userAgent: String, connections: Int, mime: String) {
        val resolver = context.contentResolver
        val headers = response.headers
        val length = Downloads.length(headers)
        val body = response.body
        notify(context, job, progress(context, 0, length, 0))
        val values = ContentValues().apply {
            put(MediaStore.Downloads.DISPLAY_NAME, job.name)
            put(MediaStore.Downloads.MIME_TYPE, mime)
            put(MediaStore.Downloads.IS_PENDING, 1)
        }
        var uri: Uri? = null
        try {
            uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: throw IOException("Downloads would not take the file")
            // A compressed answer's length is not the file's: ranges are for the file as it is.
            val encoded = Downloads.header(headers, "Content-Encoding")?.trim()?.lowercase().let { it != null && it != "identity" }
            val parallel = !encoded && Downloads.parallel(length, Downloads.header(headers, "Accept-Ranges"), connections)
            val fetched = parallel && viaDaemon(context, job, uri, response, site, userAgent, connections, length, body)
            if (!fetched) viaBrowser(context, job, uri, body ?: throw IOException("the server sent no file"), length)
            resolver.update(uri, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
            finished(context, job, null)
        } catch (e: IOException) {
            uri?.let { runCatching { resolver.delete(it, null, null) } }
            finished(context, job, if (e is Cancelled) "" else e.message ?: e.toString())
        } finally {
            runCatching { body?.close() }
            jobs.remove(job.id)
        }
    }

    /**
     * Has the daemon fetch the file in parallel ranges into [uri]. False when its answer was not
     * the file the browser saw (another size or version), so the browser's own copy is used; until
     * the daemon has its first range, the browser's connection is held open, unread, for that.
     */
    private fun viaDaemon(
        context: Context, job: Job, uri: Uri, response: WebResponse, site: String, userAgent: String,
        connections: Int, length: Long?, body: InputStream?,
    ): Boolean {
        val fd = context.contentResolver.openFileDescriptor(uri, "rw") ?: throw IOException("Downloads would not open the file")
        val headers = response.headers
        val id = Native.download(
            response.uri, site, userAgent, fd.detachFd(), connections, length ?: -1,
            Downloads.header(headers, "ETag").orEmpty(), Downloads.header(headers, "Last-Modified").orEmpty(),
        )
        var browserClosed = false
        try {
            while (true) {
                if (job.cancelled) Native.cancelDownload(id)
                val (done, total, working, state) = Native.downloadState(id)
                when (state) {
                    PROBING -> Unit
                    RUNNING -> {
                        // The daemon has the file: the browser's copy is not needed.
                        if (!browserClosed) {
                            runCatching { body?.close() }
                            browserClosed = true
                        }
                        notify(context, job, progress(context, done, total.takeIf { it >= 0 }, working.toInt()), done, total)
                    }
                    DONE -> return true
                    CHANGED -> if (browserClosed) throw IOException(Native.downloadError(id)) else return false
                    CANCELLED -> throw Cancelled()
                    else -> throw IOException(Native.downloadError(id))
                }
                Thread.sleep(POLL_MS)
            }
        } finally {
            Native.forgetDownload(id)
        }
    }

    /** Copies the browser's own response into [uri], over the connection it came on. */
    private fun viaBrowser(context: Context, job: Job, uri: Uri, body: InputStream, length: Long?) {
        val out = context.contentResolver.openOutputStream(uri, "wt") ?: throw IOException("Downloads would not open the file")
        out.use {
            val buffer = ByteArray(64 shl 10)
            var done = 0L
            var shown = 0L
            while (true) {
                if (job.cancelled) throw Cancelled()
                val n = body.read(buffer)
                if (n < 0) break
                it.write(buffer, 0, n)
                done += n
                val now = System.currentTimeMillis()
                if (now - shown >= POLL_MS) {
                    notify(context, job, progress(context, done, length, 0), done, length ?: -1)
                    shown = now
                }
            }
            if (length != null && done != length) throw IOException("the connection closed early: ${Formatter.formatShortFileSize(context, done)} of ${Formatter.formatShortFileSize(context, length)}")
        }
    }

    private fun progress(context: Context, done: Long, total: Long?, connections: Int): String {
        val size = if (total != null && total > 0) {
            context.getString(R.string.download_progress, Formatter.formatShortFileSize(context, done), Formatter.formatShortFileSize(context, total))
        } else {
            Formatter.formatShortFileSize(context, done)
        }
        return if (connections > 1) context.getString(R.string.download_connections, size, connections) else size
    }

    private fun channel(context: Context): NotificationManager {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, context.getString(R.string.download_channel), NotificationManager.IMPORTANCE_LOW),
        )
        return manager
    }

    private fun notify(context: Context, job: Job, text: String, done: Long = 0, total: Long = -1) {
        val cancel = PendingIntent.getService(
            context, job.id,
            Intent(context, OmnidService::class.java).setAction(ACTION_CANCEL).putExtra(EXTRA_ID, job.id),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val percent = if (total > 0) (done * 100 / total).toInt().coerceIn(0, 100) else 0
        val notification = Notification.Builder(context, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setColor(context.getColor(R.color.glow))
            .setContentTitle(job.name)
            .setContentText(text)
            .setProgress(100, percent, total <= 0)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .addAction(Notification.Action.Builder(null, context.getString(android.R.string.cancel), cancel).build())
            .build()
        channel(context).notify(job.id, notification)
    }

    /** Done ([error] null), cancelled (empty) or failed. */
    private fun finished(context: Context, job: Job, error: String?) {
        val manager = channel(context)
        if (error == "") {
            manager.cancel(job.id)
            return
        }
        val downloads = PendingIntent.getActivity(
            context, job.id, Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = Notification.Builder(context, CHANNEL)
            .setSmallIcon(if (error == null) android.R.drawable.stat_sys_download_done else android.R.drawable.stat_notify_error)
            .setColor(context.getColor(R.color.glow))
            .setContentTitle(job.name)
            .setContentText(if (error == null) context.getString(R.string.download_done) else context.getString(R.string.download_failed, error))
            .setContentIntent(if (error == null) downloads else null)
            .setAutoCancel(true)
            .build()
        manager.notify(job.id, notification)
    }
}
