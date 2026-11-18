package com.lognara.client.ui.screens.data

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.lognara.client.R
import java.text.NumberFormat
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.util.Locale

sealed interface DataLoadState<out T> {
    data object Loading : DataLoadState<Nothing>
    data object Error : DataLoadState<Nothing>
    data class Content<T>(val value: T) : DataLoadState<T>
}

enum class DataRange(val seconds: Long, val label: Int) {
    FifteenMinutes(900, R.string.range_15m),
    OneHour(3_600, R.string.range_1h),
    TwentyFourHours(86_400, R.string.range_24h),
    SevenDays(604_800, R.string.range_7d),
}

data class TimeWindow(val from: String, val to: String)

fun timeWindow(range: DataRange, now: Instant = Instant.now()): TimeWindow = TimeWindow(
    from = DateTimeFormatter.ISO_INSTANT.format(now.minusSeconds(range.seconds)),
    to = DateTimeFormatter.ISO_INSTANT.format(now),
)

fun intervalSeconds(range: DataRange): Long = (range.seconds / 60).coerceAtLeast(1)

fun localTimestamp(value: String): String = runCatching {
    DateTimeFormatter.ofPattern("dd.MM HH:mm:ss", Locale.getDefault())
        .withZone(ZoneId.systemDefault())
        .format(Instant.parse(value))
}.getOrDefault(value)

fun formatDataNumber(value: Number, locale: Locale): String =
    NumberFormat.getNumberInstance(locale).apply { maximumFractionDigits = 1 }.format(value)

@Composable
fun RangeSelector(selected: DataRange, onSelect: (DataRange) -> Unit, modifier: Modifier = Modifier) {
    Row(modifier = modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
        DataRange.entries.forEach { range ->
            FilterChip(
                selected = selected == range,
                onClick = { onSelect(range) },
                label = { Text(stringResource(range.label)) },
            )
        }
    }
}

@Composable
fun <T> DataStateContent(
    state: DataLoadState<T>,
    isEmpty: (T) -> Boolean,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
    emptyText: String? = null,
    content: @Composable (T) -> Unit,
) {
    when (state) {
        DataLoadState.Loading -> Column(
            modifier = modifier.fillMaxWidth().padding(32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            CircularProgressIndicator()
            Text(stringResource(R.string.data_loading), color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        DataLoadState.Error -> Column(
            modifier = modifier.fillMaxWidth().padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(stringResource(R.string.data_load_error), color = MaterialTheme.colorScheme.error)
            TextButton(onClick = onRetry) { Text(stringResource(R.string.retry)) }
        }
        is DataLoadState.Content -> if (isEmpty(state.value)) {
            Text(
                text = emptyText ?: stringResource(R.string.data_empty),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = modifier.fillMaxWidth().padding(24.dp),
            )
        } else {
            Box(modifier = modifier) {
                content(state.value)
            }
        }
    }
}
