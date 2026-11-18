plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.compose.compiler)
}

val configuredVersionCode = providers.gradleProperty("lognaraVersionCode").orNull
val configuredVersionName = providers.gradleProperty("lognaraVersionName").orNull
val configuredUpdateChannel = providers.gradleProperty("lognaraUpdateChannel").orNull
val releaseProperties = listOf(configuredVersionCode, configuredVersionName, configuredUpdateChannel)
val isConfiguredRelease = releaseProperties.all { it != null }
if (releaseProperties.any { it != null } && !isConfiguredRelease) {
    throw GradleException("Задайте lognaraVersionCode, lognaraVersionName и lognaraUpdateChannel вместе")
}

val appVersionCode = configuredVersionCode?.toIntOrNull()
    ?: if (configuredVersionCode == null) 1 else throw GradleException("lognaraVersionCode должен быть Int")
val appVersionName = configuredVersionName ?: "0.0.0-local"
val appUpdateChannel = configuredUpdateChannel ?: ""
if (appVersionCode <= 0) throw GradleException("lognaraVersionCode должен быть положительным")
if (isConfiguredRelease && appVersionName.isBlank()) throw GradleException("lognaraVersionName не может быть пустым")
if (isConfiguredRelease && appUpdateChannel !in listOf("stable", "dev")) {
    throw GradleException("lognaraUpdateChannel должен быть stable или dev")
}

val signingStoreFile = providers.environmentVariable("ANDROID_SIGNING_STORE_FILE").orNull
val signingStorePassword = providers.environmentVariable("ANDROID_SIGNING_STORE_PASSWORD").orNull
val signingKeyAlias = providers.environmentVariable("ANDROID_SIGNING_KEY_ALIAS").orNull
val signingKeyPassword = providers.environmentVariable("ANDROID_SIGNING_KEY_PASSWORD").orNull
val hasReleaseSigning = listOf(signingStoreFile, signingStorePassword, signingKeyAlias, signingKeyPassword).all { it != null }

android {
    namespace = "com.lognara.client"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.lognara.client"
        minSdk = 35
        targetSdk = 37
        versionCode = appVersionCode
        versionName = appVersionName
        buildConfigField("String", "UPDATE_CHANNEL", "\"$appUpdateChannel\"")
        buildConfigField("String", "UPDATE_REPOSITORY", "\"Liis17/Lognara\"")
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    signingConfigs {
        if (hasReleaseSigning) {
            create("releaseEnv") {
                storeFile = file(signingStoreFile!!)
                storePassword = signingStorePassword
                keyAlias = signingKeyAlias
                keyPassword = signingKeyPassword
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            if (hasReleaseSigning) {
                signingConfig = signingConfigs.getByName("releaseEnv")
            }
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.view)
    implementation(libs.zxing.core)
    implementation(libs.okhttp)
    implementation(libs.okhttp.sse)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.androidx.datastore.preferences)
    implementation(libs.kotlinx.coroutines.android)

    debugImplementation(libs.androidx.compose.ui.tooling)
    testImplementation(libs.junit)
    testImplementation(libs.mockwebserver3)
    testImplementation(libs.kotlinx.coroutines.test)
}
