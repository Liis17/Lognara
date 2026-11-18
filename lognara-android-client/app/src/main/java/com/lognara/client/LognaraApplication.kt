package com.lognara.client

import android.app.Application
import com.lognara.client.data.AndroidKeystoreTokenCipher
import com.lognara.client.data.AppSettingsStore
import com.lognara.client.data.CredentialsStore
import com.lognara.client.data.GitHubReleaseUpdater
import com.lognara.client.data.LognaraApi
import com.lognara.client.data.UpdateChannel

class LognaraApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
    }
}

class AppContainer(application: Application) {
    private val resourcesContext = application.applicationContext
    val credentialsStore = CredentialsStore(application, AndroidKeystoreTokenCipher())
    val appSettingsStore = AppSettingsStore(application, UpdateChannel.from(BuildConfig.UPDATE_CHANNEL))
    val api = LognaraApi()
    val updater = GitHubReleaseUpdater(application)

    fun getString(resourceId: Int, vararg formatArgs: Any): String = resourcesContext.getString(resourceId, *formatArgs)
}
