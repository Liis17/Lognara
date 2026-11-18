package com.lognara.client.ui

import android.os.Build
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lognara.client.BuildConfig
import com.lognara.client.AppContainer
import com.lognara.client.R
import com.lognara.client.data.ApiException
import com.lognara.client.data.AppSettings
import com.lognara.client.data.AvailableUpdate
import com.lognara.client.data.DeviceSession
import com.lognara.client.data.InstallerEvent
import com.lognara.client.data.LogEvent
import com.lognara.client.data.PairError
import com.lognara.client.data.PairPayloadException
import com.lognara.client.data.PairResult
import com.lognara.client.data.PayloadIssue
import com.lognara.client.data.UpdateChannel
import com.lognara.client.data.UpdateFailure
import com.lognara.client.data.buildDeviceName
import com.lognara.client.data.decodePairPayload
import com.lognara.client.data.isCurrentUpdateRequest
import com.lognara.client.data.isManifestChannelAllowed
import java.io.IOException
import java.io.File
import java.net.SocketTimeoutException
import java.util.concurrent.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.filterNotNull
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

enum class UpdatePhase { Idle, Checking, UpToDate, Available, Downloading, PermissionRequired, Installing, Installed, Error }

data class AppUpdateUiState(
    val phase: UpdatePhase = UpdatePhase.Idle,
    val update: AvailableUpdate? = null,
    val downloadProgress: Float? = null,
    val error: String? = null,
)

enum class SettingsError { Network, Server, Redirect, InvalidResponse }
enum class LoginNotice { DeviceStillListed }

internal fun installTimeoutState(state: AppUpdateUiState, message: String): AppUpdateUiState =
    if (state.phase == UpdatePhase.Installing && state.update != null) {
        state.copy(phase = UpdatePhase.Available, error = message)
    } else {
        state
    }

internal fun isCurrentUnauthorizedSession(requestSession: DeviceSession, currentSession: DeviceSession?): Boolean =
    requestSession == currentSession

class AppViewModel(val container: AppContainer) : ViewModel() {
    private var unauthorizedSignOut: Job? = null
    private var updateCheckJob: Job? = null
    private var updateInstallJob: Job? = null
    private var installResultTimeoutJob: Job? = null
    private var updateChannelGeneration = 0L
    private var activeInstallerGeneration: Long? = null
    private var autoCheckStarted = false
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

    private val _appSettings = MutableStateFlow(
        AppSettings(channel = UpdateChannel.from(BuildConfig.UPDATE_CHANNEL)),
    )
    val appSettings = _appSettings.asStateFlow()

    private val _appUpdate = MutableStateFlow(AppUpdateUiState())
    val appUpdate = _appUpdate.asStateFlow()

    private val _showUpdatePrompt = MutableStateFlow(false)
    val showUpdatePrompt = _showUpdatePrompt.asStateFlow()

    init {
        container.api.onUnauthorized = ::handleUnauthorized
        viewModelScope.launch { restoreCredentials() }
        viewModelScope.launch {
            container.appSettingsStore.settings.collect { settings ->
                val channel = if (updateChannelGeneration == 0L) settings.channel else _appSettings.value.channel
                _appSettings.value = settings.copy(channel = channel)
                if (BuildConfig.UPDATE_CHANNEL.isNotBlank() && settings.autoCheck && !autoCheckStarted) {
                    autoCheckStarted = true
                    checkForUpdates(showStartupPrompt = true)
                }
            }
        }
        viewModelScope.launch {
            container.updater.installerEvent.filterNotNull().collect { event ->
                if (activeInstallerGeneration == updateChannelGeneration) {
                    if (event != InstallerEvent.UserActionRequired) installResultTimeoutJob?.cancel()
                    _appUpdate.update { state ->
                        when (event) {
                            InstallerEvent.UserActionRequired -> state.copy(phase = UpdatePhase.Installing, error = null)
                            InstallerEvent.Succeeded -> state.copy(phase = UpdatePhase.Installed, error = null)
                            is InstallerEvent.Failed -> state.copy(
                                phase = UpdatePhase.Error,
                                error = container.getString(event.messageResource),
                            )
                        }
                    }
                    if (event != InstallerEvent.UserActionRequired) activeInstallerGeneration = null
                }
                container.updater.clearInstallerEvent(event)
            }
        }
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

    fun setAutoCheck(enabled: Boolean) {
        _appSettings.update { it.copy(autoCheck = enabled) }
        viewModelScope.launch { container.appSettingsStore.setAutoCheck(enabled) }
    }

    fun setUpdateChannel(channel: UpdateChannel) {
        val settings = _appSettings.value
        if (settings.channel == channel) return
        updateChannelGeneration++
        activeInstallerGeneration = null
        updateCheckJob?.cancel()
        updateCheckJob = null
        updateInstallJob?.cancel()
        updateInstallJob = null
        installResultTimeoutJob?.cancel()
        installResultTimeoutJob = null
        _appSettings.update { it.copy(channel = channel) }
        _appUpdate.value = AppUpdateUiState()
        _showUpdatePrompt.value = false
        viewModelScope.launch { container.appSettingsStore.setChannel(channel) }
        if (BuildConfig.UPDATE_CHANNEL.isNotBlank() && settings.autoCheck) checkForUpdates()
    }

    fun checkForUpdates(showStartupPrompt: Boolean = false) {
        if (BuildConfig.UPDATE_CHANNEL.isBlank() || updateCheckJob?.isActive == true) return
        val channel = _appSettings.value.channel
        val generation = updateChannelGeneration
        _appUpdate.value = AppUpdateUiState(phase = UpdatePhase.Checking)
        updateCheckJob = viewModelScope.launch {
            try {
                val available = container.updater.checkForUpdate(channel)
                if (!isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    return@launch
                }
                if (available == null) {
                    _appUpdate.value = AppUpdateUiState(phase = UpdatePhase.UpToDate)
                } else {
                    _appUpdate.value = AppUpdateUiState(phase = UpdatePhase.Available, update = available)
                    if (showStartupPrompt) _showUpdatePrompt.value = true
                }
            } catch (exception: CancellationException) {
                throw exception
            } catch (failure: UpdateFailure) {
                if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    _appUpdate.value = AppUpdateUiState(
                        phase = UpdatePhase.Error,
                        error = container.getString(failure.messageResource, *failure.formatArgs.toTypedArray()),
                    )
                }
            } catch (_: IOException) {
                if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    _appUpdate.value = AppUpdateUiState(
                        phase = UpdatePhase.Error,
                        error = container.getString(R.string.app_update_error_network),
                    )
                }
            } catch (_: Exception) {
                if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    _appUpdate.value = AppUpdateUiState(
                        phase = UpdatePhase.Error,
                        error = container.getString(R.string.app_update_error_retry),
                    )
                }
            } finally {
                try {
                    container.appSettingsStore.setLastCheckAt(System.currentTimeMillis())
                } catch (exception: CancellationException) {
                    throw exception
                } catch (_: Exception) {
                    // A failed settings write must not replace the update result.
                }
            }
        }
    }

    /** Returns false when Android's unknown-app setting must be opened first. */
    fun installUpdate(): Boolean {
        if (updateInstallJob?.isActive == true) return true
        val update = _appUpdate.value.update ?: return false
        val channel = _appSettings.value.channel
        val generation = updateChannelGeneration
        if (!isManifestChannelAllowed(update.manifest.channel, channel)) {
            _appUpdate.update {
                it.copy(
                    phase = UpdatePhase.Error,
                    update = null,
                    error = container.getString(R.string.app_update_error_channel_mismatch),
                )
            }
            return true
        }
        if (!container.updater.canRequestPackageInstalls()) {
            _appUpdate.update {
                it.copy(
                    phase = UpdatePhase.PermissionRequired,
                    error = container.getString(R.string.app_update_error_permission),
                )
            }
            return false
        }

        installResultTimeoutJob?.cancel()
        installResultTimeoutJob = null
        _appUpdate.update { it.copy(phase = UpdatePhase.Downloading, downloadProgress = null, error = null) }
        updateInstallJob = viewModelScope.launch {
            try {
                val apk: File = container.updater.downloadAndVerify(update) { downloaded, total ->
                    if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                        _appUpdate.update { state ->
                            state.copy(downloadProgress = total?.takeIf { it > 0 }?.let { (downloaded.toFloat() / it).coerceIn(0f, 1f) })
                        }
                    }
                }
                if (!isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    return@launch
                }
                if (!container.updater.canRequestPackageInstalls()) {
                    _appUpdate.update {
                        it.copy(
                            phase = UpdatePhase.PermissionRequired,
                            error = container.getString(R.string.app_update_error_permission),
                            downloadProgress = null,
                        )
                    }
                    return@launch
                }
                _appUpdate.update { it.copy(phase = UpdatePhase.Installing, downloadProgress = null, error = null) }
                activeInstallerGeneration = generation
                container.updater.install(apk)
                if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration) &&
                    _appUpdate.value.phase == UpdatePhase.Installing
                ) {
                    installResultTimeoutJob = viewModelScope.launch {
                        delay(INSTALL_RESULT_TIMEOUT_MILLIS)
                        if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                            val message = container.getString(R.string.app_update_install_timeout)
                            _appUpdate.update { state -> installTimeoutState(state, message) }
                        }
                    }
                }
            } catch (exception: CancellationException) {
                throw exception
            } catch (failure: UpdateFailure) {
                if (activeInstallerGeneration == generation) activeInstallerGeneration = null
                if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    _appUpdate.update {
                        it.copy(
                            phase = UpdatePhase.Error,
                            error = container.getString(failure.messageResource, *failure.formatArgs.toTypedArray()),
                            downloadProgress = null,
                        )
                    }
                }
            } catch (_: Exception) {
                if (activeInstallerGeneration == generation) activeInstallerGeneration = null
                if (isCurrentUpdateRequest(channel, _appSettings.value.channel, generation, updateChannelGeneration)) {
                    _appUpdate.update {
                        it.copy(
                            phase = UpdatePhase.Error,
                            error = container.getString(R.string.app_update_error_install_retry),
                            downloadProgress = null,
                        )
                    }
                }
            }
        }
        return true
    }

    fun dismissUpdatePrompt() {
        _showUpdatePrompt.value = false
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

    private companion object {
        const val INSTALL_RESULT_TIMEOUT_MILLIS = 2 * 60 * 1000L
    }
}
