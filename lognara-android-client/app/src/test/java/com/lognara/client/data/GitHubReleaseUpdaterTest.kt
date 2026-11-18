package com.lognara.client.data

import android.content.Context
import android.content.ContextWrapper
import com.lognara.client.R
import java.io.File
import java.nio.file.Files
import kotlinx.coroutines.runBlocking
import okhttp3.OkHttpClient
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test

class GitHubReleaseUpdaterTest {
    @Test
    fun stableReleaseIsFoundOnSecondPageAfterOtherReleaseChannels() = runBlocking {
        val server = MockWebServer()
        val cache = Files.createTempDirectory("android-updater-test").toFile()
        server.start()
        try {
            val otherReleases = (1..30).joinToString(",") { index ->
                release("android-dev-20261118-120000-${index.toString(16).padStart(12, '0')}")
            }
            server.enqueue(MockResponse.Builder().body("[$otherReleases]").build())
            server.enqueue(
                MockResponse.Builder()
                    .body("[${release("android-20261118-130000-aaaaaaaaaaaa")}]")
                    .build(),
            )

            val context = CacheContext(cache)
            val updater = GitHubReleaseUpdater(
                context = context,
                client = OkHttpClient(),
                apiBaseUrl = server.url("/").toString().trimEnd('/'),
                repository = "owner/repo",
                installedVersionCode = 1,
                packageVerifier = ApkPackageVerifier { _, _ -> },
            )

            val failure = runCatching { updater.checkForUpdate(UpdateChannel.Stable) }.exceptionOrNull()

            assertEquals(R.string.app_update_error_release_assets, (failure as? UpdateFailure)?.messageResource)
            val firstRequest = server.takeRequest(2L, java.util.concurrent.TimeUnit.SECONDS)
            val secondRequest = server.takeRequest(2L, java.util.concurrent.TimeUnit.SECONDS)
            assertNotNull(firstRequest)
            assertEquals(
                "/repos/owner/repo/releases?per_page=30&page=1",
                firstRequest?.target,
            )
            assertEquals(
                "/repos/owner/repo/releases?per_page=30&page=2",
                secondRequest?.target,
            )
        } finally {
            server.close()
            cache.deleteRecursively()
        }
    }

    private fun release(tag: String) =
        """{"tag_name":"$tag","draft":false,"created_at":"2026-11-18T12:00:00Z","assets":[]}"""

    private class CacheContext(cache: File) : ContextWrapper(null) {
        private val contextCache = cache

        override fun getApplicationContext(): Context = this

        override fun getCacheDir(): File = contextCache
    }
}
