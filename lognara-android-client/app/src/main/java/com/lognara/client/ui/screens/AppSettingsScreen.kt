package com.lognara.client.ui.screens

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import android.provider.Settings
import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Switch
import androidx.compose.material3.SwitchDefaults
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.lognara.client.BuildConfig
import com.lognara.client.R
import com.lognara.client.data.AppSettings
import com.lognara.client.data.UpdateChannel
import com.lognara.client.ui.AppUpdateUiState
import com.lognara.client.ui.UpdatePhase
import com.lognara.client.ui.components.DetailTopBar
import com.lognara.client.ui.components.LognaraCard
import com.lognara.client.ui.components.PillButton
import com.lognara.client.ui.components.RowDivider
import com.lognara.client.ui.components.SectionLabel
import com.lognara.client.ui.components.SegmentedSelector
import com.lognara.client.ui.components.StatusPill
import com.lognara.client.ui.components.monoStyle
import com.lognara.client.ui.theme.LognaraPalette
import com.lognara.client.ui.theme.LognaraSemanticColors
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter

@Composable
fun AppSettingsScreen(
    settings: AppSettings,
    update: AppUpdateUiState,
    onBack: () -> Unit,
    onAutoCheckChanged: (Boolean) -> Unit,
    onChannelChanged: (UpdateChannel) -> Unit,
    onCheckNow: () -> Unit,
    onInstall: () -> Boolean,
) {
    val context = LocalContext.current
    val isLocalBuild = BuildConfig.UPDATE_CHANNEL.isBlank()
    val buildChannelLabel = when (BuildConfig.UPDATE_CHANNEL) {
        "stable" -> stringResource(R.string.app_channel_stable)
        "dev" -> stringResource(R.string.app_channel_dev)
        else -> stringResource(R.string.app_local_build)
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .background(LognaraPalette.Background)
            .statusBarsPadding()
            .navigationBarsPadding(),
    ) {
        DetailTopBar(title = stringResource(R.string.app_settings_title), onBack = onBack)
        Column(
            modifier = Modifier
                .weight(1f)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp)
                .padding(top = 8.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                SectionLabel(stringResource(R.string.app_settings_version_section), Modifier.padding(horizontal = 4.dp))
                LognaraCard(
                    contentPadding = PaddingValues(0.dp),
                    shape = RoundedCornerShape(14.dp),
                    verticalArrangement = Arrangement.spacedBy(0.dp),
                ) {
                    VersionValueRow(stringResource(R.string.app_version_name), BuildConfig.VERSION_NAME)
                    RowDivider()
                    VersionValueRow(stringResource(R.string.app_version_code), BuildConfig.VERSION_CODE.toString())
                    RowDivider()
                    VersionValueRow(stringResource(R.string.app_build_channel), buildChannelLabel)
                }
            }

            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                SectionLabel(stringResource(R.string.app_settings_updates_section), Modifier.padding(horizontal = 4.dp))
                LognaraCard(
                    contentPadding = PaddingValues(16.dp),
                    shape = RoundedCornerShape(14.dp),
                    verticalArrangement = Arrangement.spacedBy(16.dp),
                ) {
                    if (isLocalBuild) {
                        Text(
                            stringResource(R.string.app_local_updates_unavailable),
                            fontSize = 13.sp,
                            color = LognaraPalette.TextSecondary,
                        )
                    } else {
                        Row(
                            modifier = Modifier.fillMaxWidth(),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.SpaceBetween,
                        ) {
                            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                                Text(stringResource(R.string.app_auto_check), fontSize = 14.sp, color = LognaraPalette.Text)
                                Text(
                                    stringResource(R.string.app_auto_check_description),
                                    fontSize = 12.sp,
                                    color = LognaraPalette.TextMuted,
                                )
                            }
                            Switch(
                                checked = settings.autoCheck,
                                onCheckedChange = onAutoCheckChanged,
                                colors = SwitchDefaults.colors(
                                    checkedThumbColor = LognaraPalette.OnAccent,
                                    checkedTrackColor = LognaraPalette.Accent,
                                    checkedBorderColor = LognaraPalette.Accent,
                                    uncheckedThumbColor = LognaraPalette.TextMuted,
                                    uncheckedTrackColor = LognaraPalette.SurfaceRaised,
                                    uncheckedBorderColor = LognaraPalette.BorderStrong,
                                ),
                            )
                        }

                        Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                            Text(stringResource(R.string.app_update_channel), fontSize = 13.sp, color = LognaraPalette.TextSecondary)
                            SegmentedSelector(
                                options = listOf(UpdateChannel.Stable, UpdateChannel.Dev),
                                selected = settings.channel,
                                label = { channel -> stringResource(if (channel == UpdateChannel.Stable) R.string.app_channel_stable else R.string.app_channel_dev) },
                                onSelect = onChannelChanged,
                            )
                        }
                    }

                    if (!isLocalBuild) UpdateStatus(update)

                    val progress = update.downloadProgress
                    if (update.phase == UpdatePhase.Downloading) {
                        if (progress == null) {
                            LinearProgressIndicator(
                                modifier = Modifier.fillMaxWidth(),
                                color = LognaraPalette.Accent,
                                trackColor = LognaraPalette.SurfaceRaised,
                            )
                        } else {
                            LinearProgressIndicator(
                                progress = { progress },
                                modifier = Modifier.fillMaxWidth(),
                                color = LognaraPalette.Accent,
                                trackColor = LognaraPalette.SurfaceRaised,
                            )
                            Text(
                                stringResource(R.string.app_download_percent, (progress * 100).toInt()),
                                fontSize = 12.sp,
                                color = LognaraPalette.TextMuted,
                            )
                        }
                    }

                    if (!isLocalBuild) {
                        when {
                            update.phase == UpdatePhase.Downloading || update.phase == UpdatePhase.Installing || update.phase == UpdatePhase.Installed -> Unit
                            update.phase == UpdatePhase.Available || update.update != null -> PillButton(
                                text = stringResource(
                                    if (update.phase == UpdatePhase.PermissionRequired) R.string.app_allow_install
                                    else R.string.app_update_now,
                                ),
                                onClick = {
                                    if (!onInstall()) openUnknownSourcesSettings(context)
                                },
                                modifier = Modifier.fillMaxWidth(),
                                primary = true,
                            )
                            else -> PillButton(
                                text = stringResource(
                                    if (update.phase == UpdatePhase.Checking) R.string.app_checking
                                    else R.string.app_check_now,
                                ),
                                onClick = onCheckNow,
                                modifier = Modifier.fillMaxWidth(),
                                enabled = update.phase != UpdatePhase.Checking,
                                primary = true,
                            )
                        }
                    }

                    settings.lastCheckAtMillis?.let { checkedAt ->
                        Text(
                            stringResource(R.string.app_last_checked, formatTimestamp(checkedAt)),
                            fontSize = 12.sp,
                            color = LognaraPalette.TextMuted,
                        )
                    }
                }
            }

            TextButton(
                onClick = {
                    try {
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse("https://github.com/${BuildConfig.UPDATE_REPOSITORY}/releases")))
                    } catch (_: ActivityNotFoundException) {
                        Toast.makeText(context, R.string.app_browser_not_found, Toast.LENGTH_SHORT).show()
                    }
                },
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(stringResource(R.string.app_all_releases), color = LognaraPalette.Accent)
            }
        }
    }
}

@Composable
private fun VersionValueRow(label: String, value: String) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 13.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
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
private fun UpdateStatus(update: AppUpdateUiState) {
    val updateVersion = update.update?.manifest?.versionName.orEmpty()
    when (update.phase) {
        UpdatePhase.Idle -> Text(stringResource(R.string.app_update_idle), fontSize = 13.sp, color = LognaraPalette.TextMuted)
        UpdatePhase.Checking -> StatusPill(stringResource(R.string.app_checking), LognaraSemanticColors.statusInfo)
        UpdatePhase.UpToDate -> StatusPill(stringResource(R.string.app_up_to_date), LognaraSemanticColors.statusOk)
        UpdatePhase.Available -> StatusPill(
            stringResource(R.string.app_update_available, updateVersion),
            LognaraSemanticColors.statusWarn,
        )
        UpdatePhase.Downloading -> Text(
            stringResource(R.string.app_download_in_progress, updateVersion),
            fontSize = 13.sp,
            color = LognaraPalette.TextSecondary,
        )
        UpdatePhase.PermissionRequired -> UpdateError(update.error)
        UpdatePhase.Installing -> Text(stringResource(R.string.app_installing), fontSize = 13.sp, color = LognaraPalette.TextSecondary)
        UpdatePhase.Installed -> StatusPill(stringResource(R.string.app_installed), LognaraSemanticColors.statusOk)
        UpdatePhase.Error -> UpdateError(update.error)
    }
}

@Composable
private fun UpdateError(message: String?) {
    Text(message ?: stringResource(R.string.app_update_error), fontSize = 13.sp, color = LognaraSemanticColors.statusErr)
}

private fun formatTimestamp(timestamp: Long): String = DateTimeFormatter.ofPattern("dd.MM.yyyy HH:mm", java.util.Locale.forLanguageTag("ru"))
    .withZone(ZoneId.systemDefault())
    .format(Instant.ofEpochMilli(timestamp))

private fun openUnknownSourcesSettings(context: android.content.Context) {
    try {
        context.startActivity(
            Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${context.packageName}"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
        )
    } catch (_: ActivityNotFoundException) {
        Toast.makeText(context, R.string.app_unknown_sources_settings_not_found, Toast.LENGTH_LONG).show()
    }
}
