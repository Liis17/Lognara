package com.lognara.client

import android.app.Application
import com.lognara.client.data.AndroidKeystoreTokenCipher
import com.lognara.client.data.CredentialsStore
import com.lognara.client.data.LognaraApi

class LognaraApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
    }
}

class AppContainer(application: Application) {
    val credentialsStore = CredentialsStore(application, AndroidKeystoreTokenCipher())
    val api = LognaraApi()
}
