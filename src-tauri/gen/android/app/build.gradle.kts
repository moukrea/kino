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

android {
    // PRD §F-018 Android build parameters (locked):
    //   minSdk 24, targetSdk 34, compileSdk 34.
    compileSdk = 34
    namespace = "dev.kino.app"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "dev.kino.app"
        minSdk = 24
        targetSdk = 34
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    // PRD §F-001 + §F-018: every APK we ship is signed with the committed
    // sideload keystore so reinstalls over previous installs succeed. The
    // keystore lives at `android/keystore/kino-dev.keystore`; alias + both
    // passwords are pinned to `kino-dev` / `kinodev`. The keystore is not a
    // security control (it's committed to the repo) so hardcoding the values
    // here is intentional — see android/keystore/README.md.
    signingConfigs {
        create("release") {
            storeFile = rootProject.file("../../../android/keystore/kino-dev.keystore")
            storePassword = "kinodev"
            keyAlias = "kino-dev"
            keyPassword = "kinodev"
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
            isMinifyEnabled = true
            signingConfig = signingConfigs.getByName("release")
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

// PRD §F-018 locks `compileSdk 34`. The Tauri 2.11 scaffold defaults to
// `compileSdk 36` and pulls androidx libraries (`activity-ktx` ≥1.10,
// `webkit` ≥1.13, `lifecycle-process` ≥2.9, `appcompat` ≥1.7.1) whose newest
// majors demand `compileSdk` ≥35. To honor the PRD lock we pin each androidx
// dependency to the highest version that still compiles cleanly against
// `compileSdk 34`. If the PRD is ever revised to relax the compileSdk pin,
// restoring the Tauri-scaffold defaults is a one-line per-dep edit.
dependencies {
    implementation("androidx.webkit:webkit:1.12.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.8.7")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")
