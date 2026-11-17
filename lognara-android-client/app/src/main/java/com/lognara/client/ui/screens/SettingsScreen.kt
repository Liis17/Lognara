package com.lognara.client.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.shape.RoundedCornerShape
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.ui.SettingsError
import com.lognara.client.ui.SettingsUiState
import com.lognara.client.ui.theme.SettingsLabelStyle

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun SettingsScreen(
    session: DeviceSession,
    state: SettingsUiState,
    onBack: () -> Unit,
    onLoad: () -> Unit,
    onRetry: () -> Unit,
    onDisconnect: () -> Unit,
) {
    var showDisconnectConfirmation by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { onLoad() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.settings_title)) },
                navigationIcon = { BackButton(onClick = onBack) },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { insets ->
        Column(
            modifier = Modifier.fillMaxSize().padding(insets).padding(horizontal = 24.dp, vertical = 20.dp),
            verticalArrangement = Arrangement.spacedBy(18.dp),
        ) {
            SettingValue(stringResource(R.string.settings_server), session.serverUrl, machine = true)
            SettingValue(stringResource(R.string.settings_username), state.username ?: session.username, machine = true)
            if (state.isLoading) {
                Text(stringResource(R.string.settings_loading), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            state.error?.let { error ->
                Text(
                    text = stringResource(
                        when (error) {
                            SettingsError.Network -> R.string.error_network
                            SettingsError.Redirect -> R.string.error_server_redirect
                            SettingsError.Server -> R.string.settings_load_error
                            SettingsError.InvalidResponse -> R.string.settings_load_error
                        },
                    ),
                    color = MaterialTheme.colorScheme.error,
                )
                TextButton(onClick = onRetry) { Text(stringResource(R.string.retry)) }
            }
            Button(
                onClick = { showDisconnectConfirmation = true },
                enabled = !state.isDisconnecting,
                modifier = Modifier.fillMaxWidth(),
                shape = MaterialTheme.shapes.medium,
            ) {
                Text(stringResource(if (state.isDisconnecting) R.string.disconnecting else R.string.disconnect_device))
            }
        }
    }

    if (showDisconnectConfirmation) {
        AlertDialog(
            onDismissRequest = { showDisconnectConfirmation = false },
            title = { Text(stringResource(R.string.disconnect_confirmation_title)) },
            text = { Text(stringResource(R.string.disconnect_confirmation_message)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        showDisconnectConfirmation = false
                        onDisconnect()
                    },
                ) { Text(stringResource(R.string.disconnect_device)) }
            },
            dismissButton = {
                TextButton(onClick = { showDisconnectConfirmation = false }) {
                    Text(stringResource(R.string.cancel))
                }
            },
            shape = RoundedCornerShape(28.dp),
        )
    }
}

@Composable
private fun SettingValue(label: String, value: String, machine: Boolean) {
    Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(label, style = SettingsLabelStyle, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            value,
            style = if (machine) com.lognara.client.ui.theme.MachineTextStyle else MaterialTheme.typography.bodyLarge,
            modifier = Modifier.fillMaxWidth(),
        )
    }
}
