package com.lognara.client.data

import android.content.Context
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.util.Base64
import androidx.datastore.core.handlers.ReplaceFileCorruptionHandler
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.emptyPreferences
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStore
import com.lognara.client.BuildConfig
import java.io.IOException
import java.security.UnrecoverableKeyException
import java.util.concurrent.CancellationException
import javax.crypto.AEADBadTagException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext

private val Context.credentialsDataStore by preferencesDataStore(
    name = "device_credentials",
    corruptionHandler = ReplaceFileCorruptionHandler { emptyPreferences() },
)

class CredentialsStore(
    context: Context,
    private val tokenCipher: TokenCipher,
) {
    private val dataStore = context.applicationContext.credentialsDataStore

    suspend fun save(session: DeviceSession) = withContext(Dispatchers.IO) {
        val encrypted = tokenCipher.encrypt(session.token, session.serverUrl)
        dataStore.edit { preferences ->
            preferences[SERVER_URL] = session.serverUrl
            preferences[USERNAME] = session.username
            preferences[DEVICE_ID] = session.deviceId
            preferences[TOKEN_IV] = Base64.encodeToString(encrypted.iv, Base64.NO_WRAP)
            preferences[TOKEN_CIPHERTEXT] = Base64.encodeToString(encrypted.ciphertext, Base64.NO_WRAP)
        }
    }

    suspend fun load(): DeviceSession? = withContext(Dispatchers.IO) {
        val preferences = try {
            dataStore.data.first()
        } catch (_: IOException) {
            return@withContext null
        }
        val fields = listOf(
            preferences[SERVER_URL],
            preferences[USERNAME],
            preferences[DEVICE_ID],
            preferences[TOKEN_IV],
            preferences[TOKEN_CIPHERTEXT],
        )
        if (fields.all { it == null } || fields.any { it == null }) {
            clear()
            return@withContext null
        }

        val serverUrl = requireNotNull(preferences[SERVER_URL])
        try {
            validateServerUrl(serverUrl, BuildConfig.DEBUG)
            val iv = Base64.decode(requireNotNull(preferences[TOKEN_IV]), Base64.NO_WRAP)
            val ciphertext = Base64.decode(requireNotNull(preferences[TOKEN_CIPHERTEXT]), Base64.NO_WRAP)
            val token = tokenCipher.decrypt(EncryptedToken(iv, ciphertext), serverUrl)
            DeviceSession(
                serverUrl = serverUrl.trimEnd('/'),
                token = token,
                deviceId = requireNotNull(preferences[DEVICE_ID]),
                username = requireNotNull(preferences[USERNAME]),
            )
        } catch (_: AEADBadTagException) {
            clear()
            null
        } catch (_: KeyPermanentlyInvalidatedException) {
            clear()
            null
        } catch (_: UnrecoverableKeyException) {
            clear()
            null
        } catch (_: PairPayloadException) {
            clear()
            null
        } catch (exception: CancellationException) {
            throw exception
        } catch (exception: Exception) {
            throw CredentialsRestoreException(exception)
        }
    }

    suspend fun clear() = withContext(Dispatchers.IO) {
        try {
            dataStore.edit { preferences ->
                preferences.remove(SERVER_URL)
                preferences.remove(USERNAME)
                preferences.remove(DEVICE_ID)
                preferences.remove(TOKEN_IV)
                preferences.remove(TOKEN_CIPHERTEXT)
            }
        } finally {
            tokenCipher.clear()
        }
    }

    private companion object {
        val SERVER_URL = stringPreferencesKey("server_url")
        val USERNAME = stringPreferencesKey("username")
        val DEVICE_ID = stringPreferencesKey("device_id")
        val TOKEN_IV = stringPreferencesKey("token_iv")
        val TOKEN_CIPHERTEXT = stringPreferencesKey("token_ciphertext")
    }
}

class CredentialsRestoreException(cause: Throwable) : Exception(cause)
