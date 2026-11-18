package com.lognara.client.ui.screens

import android.content.Intent
import androidx.core.net.toUri
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.platform.LocalContext
import com.lognara.client.R
import com.lognara.client.data.DeviceSession
import com.lognara.client.ui.SettingsError
import com.lognara.client.ui.SettingsUiState
import com.lognara.client.ui.components.DetailTopBar
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.PillButton
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.SectionLabel
import com.lognara.client.ui.components.StatusPill
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors

@Composable
fun SettingsScreen(
    session: DeviceSession,
    state: SettingsUiState,
    onBack: () -> Unit,
    onLoad: () -> Unit,
    onRetry: () -> Unit,
    onDisconnect: () -> Unit,
) {
    var showDisconnectConfirmation by remember { mutableStateOf(false) }
    val context = LocalContext.current
    val username = state.username ?: session.username
    LaunchedEffect(Unit) { onLoad() }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(LognaraPalette.Background)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        DetailTopBar(title = stringResource(R.string.settings_title), onBack = onBack)

        Column(
            modifier = Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(top = 8.dp, bottom = 16.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            ProfileCard(username)

            if (state.isLoading) {
                Row(
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 4.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(16.dp),
                        color = LognaraPalette.Accent,
                        strokeWidth = 2.dp,
                    )
                    Text(stringResource(R.string.settings_loading), fontSize = 12.sp, color = LognaraPalette.TextMuted)
                }
            }

            state.error?.let { error ->
                LognaraCard(
                    contentPadding = PaddingValues(12.dp),
                    shape = RoundedCornerShape(14.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        text = stringResource(settingsErrorMessage(error)),
                        color = LognaraSemanticColors.statusErr,
                        fontSize = 13.sp,
                    )
                    TextButton(onClick = onRetry) { Text(stringResource(R.string.retry)) }
                }
            }

            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                SectionLabel(stringResource(R.string.settings_connection_section), Modifier.padding(horizontal = 4.dp))
                LognaraCard(
                    contentPadding = PaddingValues(0.dp),
                    shape = RoundedCornerShape(14.dp),
                    verticalArrangement = Arrangement.spacedBy(0.dp),
                ) {
                    SettingsValueRow(stringResource(R.string.settings_server), session.serverUrl)
                    RowDivider()
                    SettingsValueRow(stringResource(R.string.settings_username), username)
                    RowDivider()
                    WebConsoleRow(
                        onClick = {
                            context.startActivity(Intent(Intent.ACTION_VIEW, session.serverUrl.toUri()))
                        },
                    )
                }
            }

            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                SectionLabel(stringResource(R.string.settings_device_section), Modifier.padding(horizontal = 4.dp))
                LognaraCard(
                    contentPadding = PaddingValues(16.dp),
                    shape = RoundedCornerShape(14.dp),
                    verticalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    Text(
                        stringResource(R.string.settings_disconnect_description),
                        fontSize = 13.sp,
                        color = LognaraPalette.TextSecondary,
                    )
                    PillButton(
                        text = stringResource(if (state.isDisconnecting) R.string.disconnecting else R.string.disconnect_device),
                        onClick = { showDisconnectConfirmation = true },
                        modifier = Modifier.fillMaxWidth(),
                        enabled = !state.isDisconnecting,
                        contentColor = LognaraSemanticColors.statusErr,
                        borderColor = LognaraSemanticColors.statusErr.copy(alpha = 0.4f),
                        icon = R.drawable.ic_power,
                    )
                }
            }
        }

        BrandFooter(Modifier.fillMaxWidth().padding(bottom = 8.dp))
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
                    enabled = !state.isDisconnecting,
                    colors = androidx.compose.material3.ButtonDefaults.textButtonColors(
                        contentColor = LognaraSemanticColors.statusErr,
                    ),
                ) { Text(stringResource(R.string.disconnect_device)) }
            },
            dismissButton = {
                TextButton(onClick = { showDisconnectConfirmation = false }) {
                    Text(stringResource(R.string.cancel))
                }
            },
            shape = RoundedCornerShape(28.dp),
            containerColor = LognaraPalette.Surface,
            titleContentColor = LognaraPalette.Text,
            textContentColor = LognaraPalette.TextSecondary,
        )
    }
}

@Composable
private fun ProfileCard(username: String) {
    val avatarDescription = stringResource(R.string.settings_avatar_description)
    LognaraCard(
        contentPadding = PaddingValues(16.dp),
        shape = RoundedCornerShape(20.dp),
        verticalArrangement = Arrangement.spacedBy(0.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(14.dp)) {
            Box(
                modifier = Modifier
                    .size(80.dp)
                    .clip(CircleShape)
                    .background(LognaraPalette.Accent.copy(alpha = 0.16f))
                    .semantics {
                        contentDescription = "${username.take(2).lowercase()} · $avatarDescription"
                    },
                contentAlignment = Alignment.Center,
            ) {
                Text(username.take(2).lowercase(), style = monoStyle(18, FontWeight.W500), color = LognaraPalette.Accent)
            }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text(username, style = monoStyle(16, FontWeight.W500), color = LognaraPalette.Text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                StatusPill(
                    text = stringResource(R.string.settings_device_connected),
                    color = LognaraSemanticColors.statusOk,
                )
            }
        }
    }
}

@Composable
private fun SettingsValueRow(label: String, value: String) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(label, fontSize = 12.sp, color = LognaraPalette.TextMuted)
        Text(
            value,
            style = monoStyle(13),
            color = LognaraPalette.Text,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun WebConsoleRow(onClick: () -> Unit) {
    val description = stringResource(R.string.settings_open_web_console)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 52.dp)
            .clip(RoundedCornerShape(14.dp))
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 16.dp)
            .semantics(mergeDescendants = true) {
                contentDescription = description
            },
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(stringResource(R.string.settings_open_web_console), fontSize = 14.sp, fontWeight = FontWeight.W500, color = LognaraPalette.Text)
        Icon(
            painterResource(R.drawable.ic_external),
            contentDescription = null,
            tint = LognaraPalette.TextMuted,
            modifier = Modifier.size(18.dp),
        )
    }
}

@Composable
private fun BrandFooter(modifier: Modifier = Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        val logo = buildAnnotatedString {
            append("lognara")
            withStyle(SpanStyle(color = LognaraPalette.Accent)) {
                withStyle(SpanStyle(fontStyle = FontStyle.Italic)) { append("/") }
                append("dock")
            }
        }
        Text(logo, fontSize = 17.sp, fontWeight = FontWeight.W600, letterSpacing = (-0.17).sp, color = LognaraPalette.Text)
        Text(stringResource(R.string.settings_android_client), fontSize = 12.sp, color = LognaraPalette.TextMuted)
    }
}

private fun settingsErrorMessage(error: SettingsError): Int = when (error) {
    SettingsError.Network -> R.string.error_network
    SettingsError.Redirect -> R.string.error_server_redirect
    SettingsError.Server -> R.string.settings_load_error
    SettingsError.InvalidResponse -> R.string.settings_load_error
}
