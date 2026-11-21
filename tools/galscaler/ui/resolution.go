package main

import (
	"encoding/json"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"slices"
	"time"

	"github.com/egoist/mygo/ui"
)

func (r resolution) String() string { return fmt.Sprintf("%dx%d", r.Width, r.Height) }

func (r resolution) pixels() string { return fmt.Sprintf("%d × %d", r.Width, r.Height) }

func (a *app) refreshOutputDisplay() {
	if a.checkingDisplay || !(a.settingsOpen || a.editOpen && a.draft.ID != 0) {
		return
	}
	a.checkingDisplay = true
	go func() {
		size := currentOutputMode()
		a.win.Update(func() {
			a.checkingDisplay = false
			a.outputDisplay = size
		})
	}()
}

func currentOutputMode() resolution {
	if socket := os.Getenv("NIRI_SOCKET"); socket != "" {
		return niriOutputMode(socket, os.Getpid())
	}
	runtimeDir, display := os.Getenv("XDG_RUNTIME_DIR"), os.Getenv("WAYLAND_DISPLAY")
	if runtimeDir == "" || display == "" {
		return resolution{}
	}
	pattern := filepath.Join(runtimeDir, "niri."+filepath.Base(display)+".*.sock")
	sockets, _ := filepath.Glob(pattern)
	for _, socket := range sockets {
		if size := niriOutputMode(socket, os.Getpid()); size != (resolution{}) {
			return size
		}
	}
	return resolution{}
}

func niriOutputMode(socket string, pid int) resolution {
	const timeout = 300 * time.Millisecond
	conn, err := net.DialTimeout("unix", socket, timeout)
	if err != nil {
		return resolution{}
	}
	defer conn.Close()
	if conn.SetDeadline(time.Now().Add(timeout)) != nil {
		return resolution{}
	}
	encoder, decoder := json.NewEncoder(conn), json.NewDecoder(conn)
	query := func(request string, result any) bool {
		var reply struct {
			OK map[string]json.RawMessage `json:"Ok"`
		}
		return encoder.Encode(request) == nil && decoder.Decode(&reply) == nil &&
			json.Unmarshal(reply.OK[request], result) == nil
	}

	type window struct {
		PID       int
		Workspace *uint64 `json:"workspace_id"`
	}
	var windows []window
	if !query("Windows", &windows) {
		return resolution{}
	}
	i := slices.IndexFunc(windows, func(w window) bool { return w.PID == pid && w.Workspace != nil })
	if i < 0 {
		return resolution{}
	}
	type workspace struct {
		ID     uint64
		Output string
	}
	var workspaces []workspace
	if !query("Workspaces", &workspaces) {
		return resolution{}
	}
	j := slices.IndexFunc(workspaces, func(w workspace) bool { return w.ID == *windows[i].Workspace })
	if j < 0 || workspaces[j].Output == "" {
		return resolution{}
	}
	var outputs map[string]struct {
		Current *int         `json:"current_mode"`
		Modes   []resolution `json:"modes"`
		Logical struct {
			Transform string
		}
	}
	if !query("Outputs", &outputs) {
		return resolution{}
	}
	output := outputs[workspaces[j].Output]
	if output.Current == nil || *output.Current < 0 || *output.Current >= len(output.Modes) {
		return resolution{}
	}
	size := output.Modes[*output.Current]
	if size.Width <= 0 || size.Height <= 0 {
		return resolution{}
	}
	switch output.Logical.Transform {
	case "90", "270", "Flipped90", "Flipped270":
		size.Width, size.Height = size.Height, size.Width
	}
	return size
}

type resolutionOption struct{ value, label string }

func outputOptions(display resolution, inherited, saved, draft string) []resolutionOption {
	var choices []resolutionOption
	if inherited != "" {
		r, _ := parseResolution(inherited)
		choices = append(choices, resolutionOption{"", "使用全局 · " + r.pixels()})
	}
	add := func(r resolution) {
		value := r.String()
		if !slices.ContainsFunc(choices, func(o resolutionOption) bool { return o.value == value }) {
			choices = append(choices, resolutionOption{value, r.pixels()})
		}
	}
	if display != (resolution{}) {
		add(display)
	}
	for _, preset := range []resolution{{1280, 720}, {1920, 1080}, {2560, 1440}, {3840, 2160}} {
		if display == (resolution{}) || preset.Width <= display.Width && preset.Height <= display.Height {
			add(preset)
		}
	}
	for _, extra := range []string{saved, draft} {
		if r, err := parseResolution(extra); err == nil {
			add(r)
		}
	}
	return choices
}

func (a *app) outputField(c *ui.Context, d, defaults *launchDraft) {
	var saved, inherited string
	custom := &a.settingsCustomOutput
	if defaults != nil {
		saved, inherited = a.draft.SavedOutput, defaults.Output
		custom = &a.draft.CustomOutput
	} else if a.defaults.Enhancement.Output != nil {
		saved = a.defaults.Enhancement.Output.String()
	}
	choices := outputOptions(a.outputDisplay, inherited, saved, d.Output)
	options := make([]string, 0, len(choices)+1)
	const customLabel = "自定义…"
	selected := customLabel
	current := d.Output
	if r, err := parseResolution(current); err == nil {
		current = r.String()
	}
	for _, option := range choices {
		options = append(options, option.label)
		if !*custom && option.value == current {
			selected = option.label
		}
	}
	options = append(options, customLabel)

	ui.Column(c).Grow(1).MinWidth(0).Gap(6).Children(func() {
		small(c, "输出分辨率")
		if ui.Select(c.Key("output"), &selected, options).Label("输出分辨率").FillWidth().Height(40).
			Disabled(a.busy).Changed() {
			*custom = selected == customLabel
			if *custom && d.Output == "" {
				d.Output = inherited
			}
			if i := slices.Index(options, selected); i >= 0 && i < len(choices) {
				d.Output = choices[i].value
			}
		}
		if selected == customLabel {
			field(c, "custom-output", "自定义分辨率", "例如 2560×1440", &d.Output)
		}
	})
}
