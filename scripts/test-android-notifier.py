from pathlib import Path
import os
import re
import shutil
import subprocess

root = Path(__file__).resolve().parents[1]
output = root / "target/mobile-tools/notifier-test"
output.mkdir(parents=True, exist_ok=True)
cache = Path.home() / ".gradle/caches/modules-2/files-2.1"


def jar(group, name, version=None):
    base = cache / group / name
    return str(sorted((base / version if version else base).rglob(name + "-*.jar"))[-1])


stdlib = jar("org.jetbrains.kotlin", "kotlin-stdlib", "2.0.20")
annotations = jar("org.jetbrains", "annotations")
compiler = os.pathsep.join([
    jar("org.jetbrains.kotlin", "kotlin-compiler-embeddable", "2.0.20"), stdlib,
    jar("org.jetbrains.kotlin", "kotlin-script-runtime", "2.0.20"),
    jar("org.jetbrains.kotlin", "kotlin-reflect"), jar("org.jetbrains.intellij.deps", "trove4j"),
    jar("org.jetbrains.kotlinx", "kotlinx-coroutines-core-jvm"), annotations,
])
java = str(Path(os.environ["JAVA_HOME"]) / "bin" / ("java.exe" if os.name == "nt" else "java")) if os.environ.get("JAVA_HOME") else shutil.which("java")
if not java:
    raise RuntimeError("Java is required; run the Android build first")
source = (root / "client/android/DioxusNotifier.kt").read_text(encoding="utf-8")


def member(name):
    # Expression bodies continue on deeper-indented lines, so a member ends at depth 0 before one that is not.
    start = re.search(r"^    (?:private )?(?:const )?(?:fun|val) " + re.escape(name) + r"\b", source, re.M).start()
    depth, end = 0, start
    while True:
        depth += (source[end] == "{") - (source[end] == "}")
        if source[end] == "\n" and depth == 0 and not source.startswith("        ", end + 1):
            return source[start:end].strip()
        end += 1


fixture = '''
class SharedPreferences {
    val values = mutableMapOf<String, String>()
    fun getString(key: String, default: String?): String? = values[key] ?: default
    fun getBoolean(key: String, default: Boolean): Boolean = values[key]?.toBoolean() ?: default
    fun edit() = Editor(this)
    class Editor(private val owner: SharedPreferences) {
        private val pending = mutableMapOf<String, String>()
        fun putString(key: String, value: String): Editor { pending[key] = value; return this }
        fun commit(): Boolean { owner.values.putAll(pending); return true }
    }
}
'''
checks = '''
fun main() {
    val preferences = SharedPreferences()
    val dm = "discordia-dm:a:p"
    val mention = "discordia-mention:a:quic://k@h:1:c"
    check(remember(preferences, dm, 100)) { "A first alert must show" }
    check(!remember(preferences, dm, 100)) { "The same DM by the other path must stay quiet" }
    check(!remember(preferences, dm, 99)) { "An older message must stay quiet" }
    check(remember(preferences, dm, 101)) { "A newer message must show" }
    check(remember(preferences, mention, 50)) { "Another tag must not be held back" }
    check(!remember(preferences, "bad\\ntag", 1))
    check(decodeSeen(preferences.getString("seen", null)) == linkedMapOf(dm to 101L, mention to 50L)) {
        "A new process must read back what was committed"
    }
    repeat(70) { check(remember(preferences, "t$it", 1)) }
    val capped = decodeSeen(preferences.getString("seen", null))
    check(capped.size == 64) { "At most 64 tags are kept" }
    check(capped.keys.first() == "t6") { "The oldest recorded tags leave first" }
    check(remember(preferences, dm, 101)) { "An evicted tag is news again" }
    val seen = linkedMapOf("a" to 1L, "b" to 1L)
    check(admit(seen, "a", 2) && seen.keys.toList() == listOf("b", "a")) { "A refreshed tag is the newest" }
    check(decodeSeen("x y\\n5\\n 7 z\\n8 ok:tag with space") == linkedMapOf("ok:tag with space" to 8L))
    check(decodeSeen(encodeSeen(capped)) == capped)

    val cold = SharedPreferences()
    check(label(cold, "dm") == "Private messages") { "A cold process names channels in English" }
    check(label(cold, "mention") == "Mentions")
    check(label(cold, "ping") == "Notifications are working")
    cold.values["mention"] = "Menciones"
    cold.values["dm"] = " "
    check(label(cold, "mention") == "Menciones") { "The last names Rust gave must be used" }
    check(label(cold, "dm") == "Private messages")

    val switch = SharedPreferences()
    check(wanted(switch)) { "Notifications are on until the person turns them off" }
    switch.values["enabled"] = "false"
    check(!wanted(switch)) { "A switch the person turned off stays off" }
    switch.values["enabled"] = "true"
    check(wanted(switch))
    check(asksUnprompted(true, false, 33, false)) { "A switch that is on asks Android for its permission" }
    check(!asksUnprompted(true, true, 33, false)) { "Android is asked unprompted only once" }
    check(!asksUnprompted(false, false, 33, false)) { "A switch that is off asks for nothing" }
    check(!asksUnprompted(true, false, 32, false)) { "Before Android 13 there is nothing to ask" }
    check(!asksUnprompted(true, false, 34, true))
    println("Notifier dedup, cold channel label and default switch checks pass")
}
'''
members = ["DEDUP_CAP", "SEEN", "FALLBACK", "decodeSeen", "encodeSeen", "admit", "remember", "label",
           "wanted", "asksUnprompted"]
path = output / "Notifier.kt"
path.write_text(fixture + "\n".join(map(member, members)) + "\n" + checks, encoding="utf-8")
subprocess.run([java, "-cp", compiler, "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
                "-no-stdlib", "-no-reflect", "-classpath", os.pathsep.join([stdlib, annotations]),
                "-d", str(output / "classes"), str(path)], check=True)
subprocess.run([java, "-cp", os.pathsep.join([str(output / "classes"), stdlib]), "NotifierKt"], check=True)
