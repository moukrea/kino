// PRD §F-015 Android: ExoPlayer-backed `PlayerActivity` lives in this
// gradle module. The Tauri CLI auto-includes this module into the host
// app's gradle build via the `links = "tauri-plugin-kino-player"`
// directive in the parent `Cargo.toml` (the CLI walks the host's dep
// tree and emits `include(":tauri-plugin-kino-player")` plus
// `implementation(project(":tauri-plugin-kino-player"))` into
// `src-tauri/gen/android/tauri.{settings,build}.gradle.kts` on every
// `cargo tauri android build` invocation).

plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.kino.player"
    // PRD §F-018 Android build parameters (locked):
    //   minSdk 24, targetSdk 34, compileSdk 34.
    compileSdk = 34
    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }
    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
        viewBinding = true
    }
}

dependencies {
    // Tauri 2 Android plugin base class (`app.tauri.plugin.Plugin`) and
    // the `@TauriPlugin` / `@Command` annotations. The Tauri CLI
    // supplies this via the auto-generated `tauri-android` module.
    implementation(project(":tauri-android"))

    // androidx core. Versions match those pinned by the host app
    // (`src-tauri/gen/android/app/build.gradle.kts` ADR-046).
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("androidx.localbroadcastmanager:localbroadcastmanager:1.1.0")
    implementation("com.google.android.material:material:1.12.0")

    // Media3 / ExoPlayer (PRD §F-015 ADR-010, "ExoPlayer (Media3) on
    // Android via native PlayerActivity"). 1.4.1 is the latest stable
    // version compatible with compileSdk 34. Subtitle parser parity:
    //   - SRT / WebVTT: media3-exoplayer + media3-extractor
    //   - SSA/ASS basic: media3-extractor (`SsaParser`)
    //   - PGS (tier 2): media3-exoplayer (`PgsParser`)
    implementation("androidx.media3:media3-exoplayer:1.4.1")
    implementation("androidx.media3:media3-ui:1.4.1")
    implementation("androidx.media3:media3-extractor:1.4.1")
    implementation("androidx.media3:media3-decoder:1.4.1")
    implementation("androidx.media3:media3-session:1.4.1")
    // PRD §F-015 audio passthrough — depends on the `DefaultRenderersFactory`
    // which is part of `media3-exoplayer`; no extra dep needed.

    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}
