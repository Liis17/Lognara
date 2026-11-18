package com.lognara.client.data

import android.content.Context
import androidx.datastore.core.handlers.ReplaceFileCorruptionHandler
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.emptyPreferences
import androidx.datastore.preferences.core.longPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map

private val Context.appSettingsDataStore by preferencesDataStore(
    name = "app_settings",
    corruptionHandler = ReplaceFileCorruptionHandler { emptyPreferences() },
)

enum class UpdateChannel(val value: String) {
    Stable("stable"),
    Dev("dev");

    companion object {
        fun from(value: String): UpdateChannel = entries.firstOrNull { it.value == value } ?: Stable
    }
}

data class AppSettings(
    val autoCheck: Boolean = true,
    val channel: UpdateChannel = UpdateChannel.Stable,
    val lastCheckAtMillis: Long? = null,
)

class AppSettingsStore(
    context: Context,
    private val defaultChannel: UpdateChannel,
) {
    private val dataStore = context.applicationContext.appSettingsDataStore

    val settings: Flow<AppSettings> = dataStore.data.map { preferences ->
        AppSettings(
            autoCheck = preferences[AUTO_CHECK] ?: true,
            channel = UpdateChannel.from(preferences[CHANNEL] ?: defaultChannel.value),
            lastCheckAtMillis = preferences[LAST_CHECK_AT],
        )
    }

    suspend fun setAutoCheck(enabled: Boolean) {
        dataStore.edit { it[AUTO_CHECK] = enabled }
    }

    suspend fun setChannel(channel: UpdateChannel) {
        dataStore.edit { it[CHANNEL] = channel.value }
    }

    suspend fun setLastCheckAt(millis: Long) {
        dataStore.edit { it[LAST_CHECK_AT] = millis }
    }

    private companion object {
        val AUTO_CHECK = booleanPreferencesKey("auto_check")
        val CHANNEL = stringPreferencesKey("update_channel")
        val LAST_CHECK_AT = longPreferencesKey("last_check_at")
    }
}
