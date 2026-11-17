package com.lognara.client.ui.screens

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.lognara.client.R
import com.lognara.client.data.PairError
import com.lognara.client.data.PayloadIssue
import com.lognara.client.ui.ConnectionError
import com.lognara.client.ui.PairingUiState
import com.lognara.client.ui.theme.MachineTextStyle

@Composable
fun LoginScreen(
    pairing: PairingUiState,
    notice: Boolean,
    onScan: () -> Unit,
    onManual: () -> Unit,
    onClearError: () -> Unit,
) {
    LaunchedEffect(Unit) { onClearError() }
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 24.dp, vertical = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Image(
                painter = painterResource(R.drawable.lognara_icon),
                contentDescription = null,
                modifier = Modifier.height(72.dp),
            )
            Spacer(Modifier.height(20.dp))
            Text(
                text = stringResource(R.string.login_title),
                style = MaterialTheme.typography.headlineSmall,
                textAlign = TextAlign.Center,
            )
            Spacer(Modifier.height(12.dp))
            Text(
                text = stringResource(R.string.login_instructions),
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth(),
            )
            if (notice) {
                Spacer(Modifier.height(16.dp))
                ConnectionMessage(stringResource(R.string.disconnect_offline_notice))
            }
            Spacer(Modifier.height(28.dp))
            Button(
                onClick = onScan,
                enabled = !pairing.isConnecting,
                modifier = Modifier.fillMaxWidth(),
                shape = MaterialTheme.shapes.medium,
            ) {
                Text(stringResource(R.string.scan_qr))
            }
            TextButton(onClick = onManual, enabled = !pairing.isConnecting) {
                Text(stringResource(R.string.enter_code_manually))
            }
        }
    }
}

@Composable
@OptIn(ExperimentalMaterial3Api::class)
fun ManualEntryScreen(
    pairing: PairingUiState,
    onBack: () -> Unit,
    onSubmit: (String) -> Unit,
) {
    var payload by remember { mutableStateOf("") }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.manual_entry_title)) },
                navigationIcon = { BackButton(onClick = onBack) },
                colors = TopAppBarDefaults.topAppBarColors(containerColor = MaterialTheme.colorScheme.background),
            )
        },
    ) { insets ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(insets)
                .padding(horizontal = 24.dp, vertical = 16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            OutlinedTextField(
                value = payload,
                onValueChange = { payload = it },
                modifier = Modifier.fillMaxWidth(),
                label = { Text(stringResource(R.string.payload_label)) },
                textStyle = MachineTextStyle,
                minLines = 4,
                maxLines = 8,
                shape = MaterialTheme.shapes.medium,
                enabled = !pairing.isConnecting,
            )
            pairing.error?.let { ConnectionErrorMessage(it) }
            PairButton(
                pairing = pairing,
                enabled = payload.isNotBlank(),
                onClick = { onSubmit(payload) },
            )
        }
    }
}

@Composable
fun PairButton(pairing: PairingUiState, enabled: Boolean, onClick: () -> Unit) {
    Button(
        onClick = onClick,
        enabled = enabled && !pairing.isConnecting,
        modifier = Modifier.fillMaxWidth(),
        shape = MaterialTheme.shapes.medium,
    ) {
        if (pairing.isConnecting) {
            CircularProgressIndicator(
                modifier = Modifier.size(18.dp),
                color = MaterialTheme.colorScheme.onPrimary,
                strokeWidth = 2.dp,
            )
            Text(stringResource(R.string.pairing_progress), modifier = Modifier.padding(start = 10.dp))
        } else {
            Text(stringResource(R.string.connect))
        }
    }
}

@Composable
fun ConnectionErrorMessage(error: ConnectionError) {
    val message = when (error) {
        is ConnectionError.Payload -> when (error.issue) {
            PayloadIssue.InvalidBase64 -> R.string.error_payload_base64
            PayloadIssue.InvalidJson -> R.string.error_payload_json
            PayloadIssue.UnsupportedVersion -> R.string.error_payload_version
            PayloadIssue.InvalidUrl -> R.string.error_payload_url
            PayloadIssue.InsecureUrl -> R.string.error_payload_http
            PayloadIssue.InvalidCode -> R.string.error_payload_code
        }
        is ConnectionError.Pair -> when (error.error) {
            PairError.InvalidRequest -> R.string.error_invalid_request
            PairError.InvalidCode -> R.string.error_invalid_code
            PairError.DeviceLimit -> R.string.error_device_limit
            PairError.TooManyAttempts -> R.string.error_too_many_attempts
            PairError.OutdatedDock -> R.string.error_outdated_dock
            PairError.Network -> R.string.error_network
            PairError.Timeout -> R.string.error_timeout
            PairError.Redirect -> R.string.error_server_redirect
            PairError.InvalidResponse -> R.string.error_invalid_response
            PairError.Storage, PairError.Unknown -> R.string.error_generic
        }
    }
    ConnectionMessage(stringResource(message))
}

@Composable
fun ConnectionMessage(message: String) {
    Text(
        text = message,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.error,
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
fun CredentialsRestoreErrorScreen(onRetry: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        ConnectionMessage(stringResource(R.string.credentials_restore_error))
        Spacer(Modifier.height(16.dp))
        Button(onClick = onRetry, shape = MaterialTheme.shapes.medium) {
            Text(stringResource(R.string.retry))
        }
    }
}

@Composable
fun BackButton(onClick: () -> Unit) {
    IconButton(onClick = onClick) {
        Icon(
            painter = painterResource(R.drawable.ic_arrow_back),
            contentDescription = stringResource(R.string.back),
        )
    }
}
