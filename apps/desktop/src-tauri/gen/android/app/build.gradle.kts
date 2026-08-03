import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// tauri.properties is written by `tauri android build`, which cannot finish on
// Windows (it symlinks the compiled library, which Windows refuses outside
// developer mode), and it is not tracked by git either. Reading the version
// from tauri.conf.json means calling Gradle directly still stamps the right
// one instead of a stale default.
val appVersion: String = tauriProperties.getProperty("tauri.android.versionName")
    ?: Regex("\"version\"\\s*:\\s*\"([^\"]+)\"")
        .find(file("../../../tauri.conf.json").readText())
        ?.groupValues?.get(1)
    ?: "1.0.0"

// A number that only ever grows: Android refuses to install an APK whose
// version code is lower than the one already there.
val appVersionCode: Int = tauriProperties.getProperty("tauri.android.versionCode")?.toInt()
    ?: appVersion.split(".").map { it.filter(Char::isDigit).toIntOrNull() ?: 0 }
        .let { (it + listOf(0, 0, 0)).take(3) }
        .let { (major, minor, patch) -> major * 1_000_000 + minor * 1_000 + patch }

// Signing details live outside the repository: the file is pointed at by
// HUSH_ANDROID_KEYSTORE, or found next to the user's other Android keys.
val keystoreProperties = Properties().apply {
    val fromEnv = System.getenv("HUSH_ANDROID_KEYSTORE")
    val propFile = if (fromEnv != null) file(fromEnv)
        else file(System.getProperty("user.home") + "/.android/hush-keystore.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 36
    namespace = "com.fidow.hush"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "com.fidow.hush"
        minSdk = 24
        targetSdk = 36
        versionCode = appVersionCode
        versionName = appVersion
    }
    signingConfigs {
        create("release") {
            keystoreProperties.getProperty("storeFile")?.let {
                storeFile = file(it)
                storePassword = keystoreProperties.getProperty("password")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("password")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            if (keystoreProperties.getProperty("storeFile") != null) {
                signingConfig = signingConfigs.getByName("release")
            }
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")