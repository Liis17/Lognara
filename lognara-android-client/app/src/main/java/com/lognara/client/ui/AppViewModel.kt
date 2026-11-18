package com.lognara.client.ui

import android.os.Build
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.BuildConfig
import com.lognara.client.AppContainer
import com.lognara.client.data.ApiException
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.LogEvent
import com.lognara.client.data.PairError
import com.lognara.client.data.PairPayloadException
import com.lognara.client.data.PairResult
import com.lognara.client.data.PayloadIssue
import com.lognara.client.data.buildDeviceName
import com.lognara.client.data.decodePairPayload
import java.io.IOException
import java.net.SocketTimeoutException
import java.util.concurrent.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.update

data class PairingUiState(
    val isConnecting: Boolean = false,
    val error: ConnectionError? = null,
)

sealed interface ConnectionError {
    data class Payload(val issue: PayloadIssue) : ConnectionError
    data class Pair(val error: PairError) : ConnectionError
}

data class SettingsUiState(
    val isLoading: Boolean = false,
    val username: String? = null,
    val error: SettingsError? = null,
    val isDisconnecting: Boolean = false,
)

enum class SettingsError { Network, Server, Redirect, InvalidResponse }
enum class LoginNotice { DeviceStillListed }

internal fun isCurrentUnauthorizedSession(requestSession: DeviceSession, currentSession: DeviceSession?): Boolean =
    requestSession == currentSession

class AppViewModel(val container: AppContainer) : ViewModel() {
    private var unauthorizedSignOut: Job? = null
    private val _session = MutableStateFlow<DeviceSession?>(null)
    val session = _session.asStateFlow()

    private val _selectedEvents = MutableStateFlow<Map<String, LogEvent>>(emptyMap())
    val selectedEvents = _selectedEvents.asStateFlow()

    private val _issuesRefreshKey = MutableStateFlow(0)
    val issuesRefreshKey = _issuesRefreshKey.asStateFlow()

    private val _isRestoring = MutableStateFlow(true)
    val isRestoring = _isRestoring.asStateFlow()

    private val _credentialsRestoreError = MutableStateFlow(false)
    val credentialsRestoreError = _credentialsRestoreError.asStateFlow()

    private val _pairing = MutableStateFlow(PairingUiState())
    val pairing = _pairing.asStateFlow()

    private val _settings = MutableStateFlow(SettingsUiState())
    val settings = _settings.asStateFlow()

    private val _loginNotice = MutableStateFlow<LoginNotice?>(null)
    val loginNotice = _loginNotice.asStateFlow()

    init {
        container.api.onUnauthorized = ::handleUnauthorized
        viewModelScope.launch { restoreCredentials() }
    }

    private fun handleUnauthorized(requestSession: DeviceSession) {
        viewModelScope.launch(Dispatchers.Main.immediate) {
            if (!isCurrentUnauthorizedSession(requestSession, _session.value) || unauthorizedSignOut?.isActive == true) {
                return@launch
            }
            unauthorizedSignOut = viewModelScope.launch(Dispatchers.Main.immediate) { signOut() }
        }
    }

    fun retryRestoreCredentials() {
        if (!_isRestoring.value) viewModelScope.launch { restoreCredentials() }
    }

    private suspend fun restoreCredentials() {
        _isRestoring.value = true
        _credentialsRestoreError.value = false
        try {
            _session.value = container.credentialsStore.load()
        } catch (exception: CancellationException) {
            throw exception
        } catch (_: Exception) {
            _credentialsRestoreError.value = true
        } finally {
            _isRestoring.value = false
        }
    }

    fun clearPairingError() {
        if (!_pairing.value.isConnecting) _pairing.value = PairingUiState()
    }

    fun selectEvent(event: LogEvent) {
        _selectedEvents.update { it + (event.id to event) }
    }

    fun refreshIssues() {
        _issuesRefreshKey.value += 1
    }

    fun connect(payloadText: String) {
        if (_pairing.value.isConnecting) return
        _loginNotice.value = null
        _pairing.value = PairingUiState(isConnecting = true)
        viewModelScope.launch {
            val payload = try {
                decodePairPayload(payloadText, allowInsecureLocalhost = BuildConfig.DEBUG)
            } catch (exception: PairPayloadException) {
                _pairing.value = PairingUiState(error = ConnectionError.Payload(exception.issue))
                return@launch
            }

            when (val result = container.api.pair(payload, buildDeviceName(Build.MANUFACTURER, Build.MODEL))) {
                is PairResult.Failure -> _pairing.value = PairingUiState(error = ConnectionError.Pair(result.error))
                is PairResult.Success -> {
                    val session = DeviceSession(
                        serverUrl = payload.url,
                        token = result.response.token,
                        deviceId = result.response.device_id,
                        username = result.response.username,
                    )
                    try {
                        container.credentialsStore.save(session)
                        _session.value = session
                        _pairing.value = PairingUiState()
                    } catch (_: Exception) {
                        _pairing.value = PairingUiState(error = ConnectionError.Pair(PairError.Storage))
                    }
                }
            }
        }
    }

    fun loadSettings() {
        val session = _session.value ?: return
        _settings.value = SettingsUiState(isLoading = true)
        viewModelScope.launch {
            try {
                val me = container.api.getMe(session)
                _settings.value = SettingsUiState(username = me.username)
            } catch (_: ApiException.Unauthorized) {
                // LognaraApi handles every authenticated 401 through handleUnauthorized.
            } catch (_: ApiException.InvalidResponse) {
                _settings.value = SettingsUiState(error = SettingsError.InvalidResponse)
            } catch (_: ApiException.Redirect) {
                _settings.value = SettingsUiState(error = SettingsError.Redirect)
            } catch (_: ApiException.Server) {
                _settings.value = SettingsUiState(error = SettingsError.Server)
            } catch (_: SocketTimeoutException) {
                _settings.value = SettingsUiState(error = SettingsError.Network)
            } catch (_: IOException) {
                _settings.value = SettingsUiState(error = SettingsError.Network)
            }
        }
    }

    fun disconnect() {
        val session = _session.value ?: return
        if (_settings.value.isDisconnecting) return
        _settings.value = _settings.value.copy(isDisconnecting = true)
        viewModelScope.launch {
            var deviceMayRemain = false
            try {
                container.api.disconnect(session)
            } catch (_: ApiException.Unauthorized) {
                handleUnauthorized(session)
                return@launch
            } catch (_: ApiException.Redirect) {
                _settings.value = SettingsUiState(error = SettingsError.Redirect)
                return@launch
            } catch (_: Exception) {
                deviceMayRemain = true
            }
            signOut()
            _loginNotice.value = if (deviceMayRemain) LoginNotice.DeviceStillListed else null
        }
    }

    private suspend fun signOut() {
        try {
            container.credentialsStore.clear()
        } finally {
            _session.value = null
            _selectedEvents.value = emptyMap()
            _settings.value = SettingsUiState()
        }
    }
}
