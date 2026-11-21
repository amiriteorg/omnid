# JNI binds Java_cc_wckd_omnid_Native_* by name.
-keep class cc.wckd.omnid.Native {
    native <methods>;
}

# GeckoView's WebAuthn support refers to Google Play services, which is left
# out of the app (security.webauth.webauthn is off).
-dontwarn com.google.android.gms.**

# GeckoView reads its config file (the browser's prefs) with SnakeYAML, which
# looks up its own package by name; repackaged, it fails before Gecko starts.
-keep class org.yaml.snakeyaml.** { *; }
-dontwarn org.yaml.snakeyaml.**
