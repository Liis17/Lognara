package com.lognara.client.ui.theme

import androidx.compose.material3.Typography
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontVariation
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import com.lognara.client.R

private fun commissionerFont(weight: FontWeight) = Font(
    R.font.commissioner,
    weight = weight,
    variationSettings = FontVariation.Settings(FontVariation.weight(weight.weight)),
)

private val Commissioner = FontFamily(
    commissionerFont(FontWeight.W100),
    commissionerFont(FontWeight.W200),
    commissionerFont(FontWeight.W300),
    commissionerFont(FontWeight.W400),
    commissionerFont(FontWeight.W500),
    commissionerFont(FontWeight.W600),
    commissionerFont(FontWeight.W700),
    commissionerFont(FontWeight.W800),
    commissionerFont(FontWeight.W900),
)

val LognaraMono = FontFamily(
    Font(R.font.ibm_plex_mono_regular, FontWeight.W400),
    Font(R.font.ibm_plex_mono_medium, FontWeight.W500),
)

val MachineTextStyle = TextStyle(
    fontFamily = LognaraMono,
    fontSize = 14.sp,
    lineHeight = 21.sp,
)

val SettingsLabelStyle = TextStyle(
    fontFamily = Commissioner,
    fontSize = 11.sp,
    lineHeight = 16.sp,
    fontWeight = FontWeight.W600,
    letterSpacing = 0.77.sp,
)

val LognaraTypography = Typography(
    displayLarge = TextStyle(fontFamily = Commissioner, fontSize = 28.sp, lineHeight = 34.sp, fontWeight = FontWeight.W600, letterSpacing = (-0.28).sp),
    displayMedium = TextStyle(fontFamily = Commissioner, fontSize = 26.sp, lineHeight = 33.sp, fontWeight = FontWeight.W600),
    displaySmall = TextStyle(fontFamily = Commissioner, fontSize = 24.sp, lineHeight = 30.sp, fontWeight = FontWeight.W600),
    headlineLarge = TextStyle(fontFamily = Commissioner, fontSize = 26.sp, lineHeight = 36.sp, fontWeight = FontWeight.W600),
    headlineSmall = TextStyle(fontFamily = Commissioner, fontSize = 24.sp, lineHeight = 36.sp, fontWeight = FontWeight.W600),
    headlineMedium = TextStyle(fontFamily = Commissioner, fontSize = 22.sp, lineHeight = 33.sp, fontWeight = FontWeight.W600),
    titleLarge = TextStyle(fontFamily = Commissioner, fontSize = 16.sp, lineHeight = 24.sp, fontWeight = FontWeight.W600),
    titleMedium = TextStyle(fontFamily = Commissioner, fontSize = 15.sp, lineHeight = 23.sp, fontWeight = FontWeight.W600),
    titleSmall = TextStyle(fontFamily = Commissioner, fontSize = 14.sp, lineHeight = 21.sp, fontWeight = FontWeight.W600),
    bodyLarge = TextStyle(fontFamily = Commissioner, fontSize = 14.sp, lineHeight = 21.sp, fontWeight = FontWeight.W400),
    bodyMedium = TextStyle(fontFamily = Commissioner, fontSize = 13.sp, lineHeight = 20.sp, fontWeight = FontWeight.W400),
    bodySmall = TextStyle(fontFamily = Commissioner, fontSize = 12.sp, lineHeight = 18.sp, fontWeight = FontWeight.W400),
    labelLarge = TextStyle(fontFamily = Commissioner, fontSize = 14.sp, lineHeight = 20.sp, fontWeight = FontWeight.W600, letterSpacing = 0.1.sp),
    labelMedium = TextStyle(fontFamily = Commissioner, fontSize = 12.sp, lineHeight = 16.sp, fontWeight = FontWeight.W600, letterSpacing = 0.5.sp),
    labelSmall = TextStyle(fontFamily = Commissioner, fontSize = 11.sp, lineHeight = 16.sp, fontWeight = FontWeight.W500, letterSpacing = 0.5.sp),
)
