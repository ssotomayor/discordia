from pathlib import Path
import os
import re
import shutil
import subprocess

root = Path(__file__).resolve().parents[1]
output = root / "target/mobile-tools/notification-dispatch-test"
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
source = (root / "client/android/MainActivity.kt").read_text(encoding="utf-8")


def method(name):
    start = re.search(r"    (?:private )?fun " + re.escape(name) + r"\(", source).start()
    opening = source.index("{", start)
    depth, end = 1, opening + 1
    while depth:
        depth += (source[end] == "{") - (source[end] == "}")
        end += 1
    return source[start:end]


fixture = '''
val operations = mutableListOf<String>()
var failures = 0
object android { object util { object Log {
    fun w(tag: String, message: String, error: Exception) { failures++ }
}}}
class DmNotifications {
    fun show(value: String) {
        if (value == "invalid") throw IllegalArgumentException("invalid notification")
        operations.add("show:$value")
    }
    fun dismiss(value: String) { operations.add("dismiss:$value") }
    fun dismissRead(value: String) { operations.add("read:$value") }
    fun clearAccount(value: String) { operations.add("clear:$value") }
}
object com { object discordia { object app { object DioxusForegroundService {
    fun updateControls(value: String) { operations.add("call:$value") }
}}}}
class Harness {
    val dmNotifications = DmNotifications()
    val ui = ArrayDeque<() -> Unit>()
    var voiceSlot = 7
    var voiceServiceResult = 1
    fun runOnUiThread(action: () -> Unit) { ui.addLast(action) }
    fun updateMediaService() { operations.add("stop:$voiceSlot") }
    fun drain() { while(ui.isNotEmpty()) ui.removeFirst().invoke() }
'''
checks = '''
}
fun main() {
    val activity = Harness()
    repeat(100) { activity.showDmNotification("message-$it") }
    activity.updateCallNotification("muted")
    activity.updateCallNotification("unmuted")
    activity.dismissReadDmNotification("read-cutoff")
    activity.dismissDmNotification("peer")
    activity.clearDmNotificationAccount("account")
    activity.showDmNotification("invalid")
    activity.showDmNotification("after-error")
    activity.stopVoiceService(6)
    check(operations.isEmpty()) { "Native callers must return before UI work runs" }
    check(activity.voiceSlot == 7)
    activity.drain()
    check(operations.take(100) == (0 until 100).map { "show:message-$it" })
    check(operations.drop(100) == listOf("call:muted", "call:unmuted", "read:read-cutoff", "dismiss:peer", "clear:account", "show:after-error"))
    check(failures == 1) { "An asynchronous platform error must not crash the UI queue" }
    check(activity.voiceSlot == 7) { "A stale hangup must preserve a replacement call" }
    activity.stopVoiceService(7)
    activity.drain()
    check(activity.voiceSlot == -1 && activity.voiceServiceResult == 0)
    check(operations.last() == "stop:-1")
    println("Background notification queue and call teardown checks pass")
}
'''
methods = ["postNotification", "showDmNotification", "updateCallNotification", "dismissReadDmNotification",
           "dismissDmNotification", "clearDmNotificationAccount", "stopVoiceService"]
path = output / "NotificationDispatch.kt"
path.write_text(fixture + "\n".join(map(method, methods)) + checks, encoding="utf-8")
subprocess.run([java, "-cp", compiler, "org.jetbrains.kotlin.cli.jvm.K2JVMCompiler",
                "-no-stdlib", "-no-reflect", "-classpath", os.pathsep.join([stdlib, annotations]),
                "-d", str(output / "classes"), str(path)], check=True)
subprocess.run([java, "-cp", os.pathsep.join([str(output / "classes"), stdlib]),
                "NotificationDispatchKt"], check=True)
