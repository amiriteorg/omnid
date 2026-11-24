import java.util.Properties

plugins {
    id("com.android.application")
}

android {
    namespace = "cc.wckd.omnid"
    compileSdk {
        // 37.2: GeckoView compiles against it.
        version = release(37) {
            minorApiLevel = 2
        }
    }

    defaultConfig {
        applicationId = "cc.wckd.omnid"
        minSdk = 29
        targetSdk = 37
        versionCode = 9
        versionName = "0.1.8"
        // The Rust library is built for arm64 only (phones and Apple-silicon
        // emulators); see build-rust.sh.
        ndk {
            abiFilters += "arm64-v8a"
        }
    }

    // Release builds are signed with keystore.properties (git-ignored) when
    // it exists, and with the debug key otherwise, which is fine for
    // sideloading and not for distribution.
    val keystoreFile = rootProject.file("keystore.properties")
    val releaseSigning = if (keystoreFile.isFile) {
        val props = Properties().apply { keystoreFile.inputStream().use(::load) }
        signingConfigs.create("release") {
            storeFile = file(props.getProperty("storeFile"))
            storePassword = props.getProperty("storePassword")
            keyAlias = props.getProperty("keyAlias")
            keyPassword = props.getProperty("keyPassword")
            // v3 carries a proof-of-rotation, so a replaced key can still
            // update existing installs.
            enableV3Signing = true
        }
    } else {
        signingConfigs.getByName("debug")
    }

    buildTypes {
        getByName("release") {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            signingConfig = releaseSigning
            vcsInfo {
                include = false
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    // Native libraries compressed in the APK and unpacked on install: with GeckoView the APK is
    // 103 MB this way and 218 MB stored, and it is downloaded directly, not through a store.
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }

    dependenciesInfo {
        includeInApk = false
        includeInBundle = false
    }
}

dependencies {
    // The browser's engine (MPL-2.0). Update with every Firefox release: it carries the security
    // fixes. Google Play services is left out; WebAuthn, which needs it, is turned off.
    implementation("org.mozilla.geckoview:geckoview-arm64-v8a:161.0.20261118180411") {
        exclude(group = "com.google.android.gms")
    }
    testImplementation("junit:junit:4.13.2")
}
