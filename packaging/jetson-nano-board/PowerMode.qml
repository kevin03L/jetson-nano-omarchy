import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui

// style (shell.json): "watts" (default), "label" or "icon"
BarWidget {
  id: root
  moduleName: "jetson.power-mode"

  property string mode: ""
  property string label: ""
  property string watts: ""
  property string details: ""
  readonly property string style: settings && settings.style ? settings.style : "watts"
  readonly property var icons: ({ eco: "", normal: "", perf: "", max: "" })

  function refresh() {
    if (!proc.running) proc.running = true
  }

  visible: mode !== ""
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  Process {
    id: proc
    command: ["jetson-nano-mode", "bar"]
    stdout: StdioCollector {
      onStreamFinished: {
        var lines = text.trim().split("\n")
        root.mode = lines[0] || ""
        root.label = lines[1] || ""
        root.watts = lines[2] || ""
        root.details = lines.slice(3).join("\n")
      }
    }
  }

  Timer {
    interval: 5000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: root.refresh()
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.icons[root.mode] || ""
    labelText: root.style === "label" ? root.label : root.style === "watts" ? root.watts : ""
    slotSize: root.style === "icon" ? Style.bar.statusSlot : Style.bar.statusSlot + Style.space(root.style === "label" ? 62 : 44)
    active: root.mode === "max"
    useActiveColor: false
    tooltipText: root.details
    onPressed: if (root.bar) root.bar.run("jetson-nano-mode menu")
  }
}
