import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// Release signing is supplied out of band: android/key.properties points at the
// keystore and is ignored by git. Keeping the file optional preserves the
// Flutter template's `flutter run --release` behaviour for developers, while
// tools/release/build-android.sh refuses to produce a "release" artifact
// without it.
val keystorePropertiesFile = rootProject.file("key.properties")
val keystoreProperties = Properties()
val hasReleaseKeystore = keystorePropertiesFile.isFile
if (hasReleaseKeystore) {
    FileInputStream(keystorePropertiesFile).use { keystoreProperties.load(it) }
}

android {
    namespace = "com.lingxing.unwrittendiary"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    // cargo-ndk writes ABI directories below this folder. Gradle packages the
    // corresponding libdiary_bridge.so inside the APK/AAB.
    sourceSets.getByName("main").jniLibs.srcDirs(file("../../../../target/android-jniLibs"))

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // Final product identity. Keep this stable after users install the app.
        applicationId = "com.lingxing.unwrittendiary"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        // Uses the version code from pubspec.yaml. When using split APKs, 1000 * ABI_VERSION
        // is added automatically by Flutter. (https://developer.android.com/studio/build/configure-apk-splits#configure-APK-versions)
        // You can force using the value of versionCode by specifying the `-P force-version-code-ignoring-abi=true`
        // flag during build.
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    signingConfigs {
        if (hasReleaseKeystore) {
            create("release") {
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
                // Paths in key.properties are relative to the android/ project
                // directory, matching Flutter's documented convention.
                storeFile = rootProject.file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
            }
        }
    }

    buildTypes {
        release {
            // With key.properties present, release builds are signed with the
            // release keystore. Without it, keep the Flutter template's debug
            // signing so `flutter run --release` still works locally; the
            // release script treats that fallback as a hard failure.
            signingConfig = if (hasReleaseKeystore) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
            // The Flutter Gradle plugin turns R8 on for release builds and
            // adds its own proguard rules. See android/app/proguard-rules.pro
            // for the app-specific additions.
        }
    }
}

val rustBridgeArm64 = file("../../../../target/android-jniLibs/arm64-v8a/libdiary_bridge.so")
val verifyRustBridge by tasks.registering {
    doLast {
        if (!rustBridgeArm64.isFile) {
            throw GradleException("Missing Android arm64 diary bridge: $rustBridgeArm64. Build it with cargo-ndk before making a release.")
        }
    }
}
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    dependsOn(verifyRustBridge)
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
