package main

import (
	"bufio"
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

type object = map[string]any
type resolution struct {
	Width  int `json:"width"`
	Height int `json:"height"`
}
type enhancement struct {
	Enabled    *bool       `json:"enabled,omitempty"`
	Filter     string      `json:"filter,omitempty"`
	Source     *resolution `json:"source,omitempty"`
	Output     *resolution `json:"output,omitempty"`
	Fullscreen *bool       `json:"fullscreen,omitempty"`
	FPS        *int        `json:"fps,omitempty"`
}
type launchSettings struct {
	Proton      string             `json:"proton,omitempty"`
	Enhancement enhancement        `json:"enhancement"`
	Environment *map[string]string `json:"environment,omitempty"`
}
type launchDraft struct {
	Proton, Enabled, Filter, Source, Output, Fullscreen, FPS, Environment string
	CustomProton                                                          bool
}
type game struct {
	ID         uint64   `json:"id"`
	Name       string   `json:"name"`
	Executable string   `json:"executable"`
	Cover      *string  `json:"cover"`
	Arguments  []string `json:"arguments"`
	Prefix     string   `json:"prefix"`
	launchSettings
}
type session struct {
	ID      uint64 `json:"game_id"`
	State   string `json:"state"`
	Error   string `json:"error"`
	Metrics struct {
		DesktopHidden bool `json:"desktop_hidden"`
	} `json:"metrics"`
}

func (s session) canResume() bool {
	return s.State == "running" && s.Metrics.DesktopHidden
}

type proton struct {
	Name string `json:"name"`
	Path string `json:"path"`
}
type diagnostics struct {
	Wayland         string   `json:"wayland"`
	UmuRun          string   `json:"umu_run"`
	SystemdUser     bool     `json:"systemd_user"`
	Runtimes        []proton `json:"runtimes"`
	AutomaticProton string   `json:"automatic_proton"`
	GeTools         bool     `json:"ge_tools"`
}
type lanStatus struct {
	Name        string `json:"name"`
	Enabled     bool   `json:"enabled"`
	Address     string `json:"address"`
	HasPassword bool   `json:"has_password"`
	Error       string `json:"error"`
}
type inspection struct {
	Directory  string   `json:"directory"`
	Candidates []string `json:"candidates"`
	Selected   string   `json:"selected"`
}
type coverCandidate struct {
	Title     string  `json:"title"`
	Released  *string `json:"released"`
	Release   bool    `json:"release"`
	URL       string  `json:"url"`
	Thumbnail *string `json:"thumbnail"`
}

type editorMode uint8

const (
	addGame editorMode = iota
	editGame
	editQuality
)

type editor struct {
	ID                                        uint64
	Name, Executable, Cover, Arguments, Error string
	Prefix, SavedOutput                       string
	CustomOutput                              bool
	Launch                                    launchDraft
	Directory                                 string
	Scan                                      *inspection
	Pending, LaunchTab, Expert, CoverEdited   bool
	CoverSearch                               string
	CoverCandidates                           []coverCandidate
}

type app struct {
	win                            *mygo.Window
	games                          []game
	sessions                       []session
	runtimes                       []proton
	sharedProton                   string
	geLatest                       *string
	geAction, geError              string
	selected                       uint64
	query                          string
	settingsOpen                   bool
	settingsTab                    int
	connected, busy, polling       bool
	refreshAgain                   bool
	connectionError                string
	err, log                       string
	diagnostics                    *diagnostics
	checking, settingsAdvanced     bool
	defaults                       *launchSettings
	settingsDraft                  launchDraft
	settingsCustomOutput           bool
	settingsSaved                  time.Time
	lan                            *lanStatus
	lanError, lanPassword          string
	lanPasswordOpen, lanEnable     bool
	lanCopied                      time.Time
	outputDisplay                  resolution
	checkingDisplay                bool
	appearance                     appearance
	editOpen, confirmOpen, logOpen bool
	draft                          editor
	editorVersion                  uint64
	images                         map[string]ui.ImageSource
	sakura, foliage                ui.ImageSource
	density                        int
	listView, focusShelf           bool
	libraryScroll                  ui.ScrollState
	revealSelection                bool
	copiedAt                       time.Time
	focusSearch                    bool
}

const frameLimit = 1024 * 1024

var daemonStart sync.Mutex

func dialDaemon(ctx context.Context) (net.Conn, error) {
	data := os.Getenv("GALSCALER_DATA_DIR")
	if data == "" {
		share := os.Getenv("XDG_DATA_HOME")
		if share == "" {
			home, err := os.UserHomeDir()
			if err != nil {
				return nil, err
			}
			share = filepath.Join(home, ".local", "share")
		}
		data = filepath.Join(share, "galscaler")
	}
	socket := os.Getenv("GALSCALER_SOCKET")
	if socket == "" {
		dir := os.Getenv("XDG_RUNTIME_DIR")
		if dir == "" {
			dir = data
		}
		socket = filepath.Join(dir, "galscaler.sock")
	}

	dialer := net.Dialer{}
	conn, err := dialer.DialContext(ctx, "unix", socket)
	if !errors.Is(err, os.ErrNotExist) && !errors.Is(err, syscall.ECONNREFUSED) {
		return conn, err
	}

	daemonStart.Lock()
	defer daemonStart.Unlock()
	conn, err = dialer.DialContext(ctx, "unix", socket)
	if !errors.Is(err, os.ErrNotExist) && !errors.Is(err, syscall.ECONNREFUSED) {
		return conn, err
	}
	bin := os.Getenv("GALSCALER_DAEMON")
	if bin == "" {
		bin = "galscalerd"
		if exe, err := os.Executable(); err == nil {
			sibling := filepath.Join(filepath.Dir(exe), bin)
			if _, err := os.Stat(sibling); err == nil {
				bin = sibling
			}
		}
	}
	output, err := exec.CommandContext(ctx, bin, "--data-dir", data, "--socket", socket, "--background").CombinedOutput()
	if err != nil {
		if message := strings.TrimSpace(string(output)); message != "" {
			return nil, fmt.Errorf("%s", message)
		}
		return nil, fmt.Errorf("无法启动 galscalerd：%w", err)
	}
	return dialer.DialContext(ctx, "unix", socket)
}

func call(request object) (json.RawMessage, error) {
	wait := 15 * time.Second
	switch request["op"] {
	case "find_covers", "check_ge_proton":
		wait = 30 * time.Second
	case "install_ge_proton":
		wait = 62 * time.Minute
	}
	ctx, cancel := context.WithTimeout(context.Background(), wait)
	defer cancel()
	switch request["op"] {
	case "launch", "doctor":
		desktop := object{}
		if display := os.Getenv("DISPLAY"); display != "" {
			desktop["display"] = display
		}
		if path := os.Getenv("WAYLAND_DISPLAY"); path != "" {
			if dir := os.Getenv("XDG_RUNTIME_DIR"); !filepath.IsAbs(path) && dir != "" {
				path = filepath.Join(dir, path)
			}
			if filepath.IsAbs(path) {
				if _, err := os.Stat(path); err == nil {
					desktop["wayland_display"] = path
				}
			}
		}
		request["desktop"] = desktop
	}
	payload, err := json.Marshal(request)
	if err != nil {
		return nil, err
	}
	if len(payload) >= frameLimit {
		return nil, fmt.Errorf("请求超过 1 MiB")
	}
	conn, err := dialDaemon(ctx)
	if err != nil {
		return nil, fmt.Errorf("无法连接后端：%w", err)
	}
	defer conn.Close()
	deadline, _ := ctx.Deadline()
	if err := conn.SetDeadline(deadline); err != nil {
		return nil, err
	}
	if _, err := conn.Write(append(payload, '\n')); err != nil {
		return nil, err
	}
	data, err := bufio.NewReader(io.LimitReader(conn, frameLimit+1)).ReadBytes('\n')
	if len(data) > frameLimit {
		return nil, fmt.Errorf("响应超过 1 MiB")
	}
	if err != nil {
		return nil, fmt.Errorf("无法读取后端响应：%w", err)
	}
	var reply struct {
		OK     bool            `json:"ok"`
		Result json.RawMessage `json:"result"`
		Error  string          `json:"error"`
	}
	if err := json.Unmarshal(data, &reply); err != nil {
		return nil, fmt.Errorf("后端返回了无效数据：%w", err)
	}
	if !reply.OK {
		return nil, errors.New(reply.Error)
	}
	return reply.Result, nil
}

func (a *app) refresh() {
	a.refreshOutputDisplay()
	if a.polling || a.busy {
		return
	}
	a.polling = true
	readLan := a.settingsOpen && a.settingsTab == 2
	go func() {
		data, gerr := call(object{"op": "library"})
		status, serr := call(object{"op": "status"})
		var lan lanStatus
		var lanErr error
		if readLan {
			var value json.RawMessage
			value, lanErr = call(object{"op": "lan"})
			if lanErr == nil {
				lanErr = json.Unmarshal(value, &lan)
			}
		}
		a.win.Update(func() {
			a.polling = false
			if a.refreshAgain {
				a.refreshAgain = false
				a.refresh()
				return
			}
			if gerr != nil {
				a.connected = false
				a.connectionError = gerr.Error()
				return
			}
			if readLan {
				if lanErr != nil {
					a.lanError = lanErr.Error()
				} else {
					a.lan, a.lanError = &lan, ""
				}
			}
			var library struct {
				Games        []game         `json:"games"`
				Settings     launchSettings `json:"settings"`
				SharedProton string         `json:"shared_proton"`
			}
			if err := json.Unmarshal(data, &library); err != nil {
				a.connected = false
				a.connectionError = err.Error()
				return
			}
			var state struct {
				Sessions []session `json:"sessions"`
			}
			if serr == nil {
				serr = json.Unmarshal(status, &state)
			}
			a.games = library.Games
			a.sharedProton = library.SharedProton
			if a.defaults == nil || a.settingsDraft == form(*a.defaults) {
				a.settingsDraft = form(library.Settings)
			}
			a.defaults = &library.Settings
			if a.editOpen && a.draft.ID != 0 && !a.draft.CoverEdited {
				if i := slices.IndexFunc(a.games, func(g game) bool { return g.ID == a.draft.ID }); i >= 0 {
					a.draft.Cover = ""
					if a.games[i].Cover != nil {
						a.draft.Cover = *a.games[i].Cover
					}
				}
			}
			reconnected := !a.connected && serr == nil
			a.connected = serr == nil
			if serr != nil {
				a.connectionError = serr.Error()
			} else {
				a.connectionError = ""
				a.sessions = state.Sessions
			}
			a.selectVisible()
			if reconnected {
				a.loadRuntimes()
			}
		})
	}()
}

func (a *app) reload() {
	a.err = ""
	a.refresh()
	if a.settingsOpen {
		a.checkEnvironment()
	}
}

func (a *app) loadRuntimes() {
	go func() {
		value, err := call(object{"op": "runtimes"})
		a.win.Update(func() {
			if err != nil {
				a.err = err.Error()
				return
			}
			if err = json.Unmarshal(value, &a.runtimes); err != nil {
				a.err = err.Error()
			}
		})
	}()
}

func (a *app) checkEnvironment() {
	if a.checking {
		return
	}
	a.checking = true
	go func() {
		data, err := call(object{"op": "doctor"})
		var result diagnostics
		if err == nil {
			err = json.Unmarshal(data, &result)
		}
		a.win.Update(func() {
			a.checking = false
			if err != nil {
				a.diagnostics = nil
				if a.settingsOpen {
					a.err = err.Error()
				}
				return
			}
			a.diagnostics = &result
			a.runtimes = result.Runtimes
		})
	}()
}

func (a *app) manageProton(install bool) {
	if !a.connected || a.geAction != "" {
		return
	}
	request := object{"op": "check_ge_proton"}
	a.geAction = "正在检查更新…"
	if install {
		request["op"] = "install_ge_proton"
		if a.geLatest != nil && *a.geLatest != "" {
			request["version"] = *a.geLatest
		}
		a.geAction = "正在下载和安装…"
	}
	a.geError = ""
	go func() {
		data, err := call(request)
		var latest string
		if err == nil && !install {
			err = json.Unmarshal(data, &latest)
		}
		a.win.Update(func() {
			a.geAction = ""
			if err != nil {
				a.geError = err.Error()
			} else if install {
				a.geLatest = nil
				a.checkEnvironment()
			} else {
				a.geLatest = &latest
			}
		})
	}()
}

func (a *app) send(request object) {
	if a.busy {
		return
	}
	a.busy = true
	a.err = ""
	go func() {
		value, err := call(request)
		a.win.Update(func() {
			a.busy = false
			if err != nil {
				a.err = err.Error()
				return
			}
			switch request["op"] {
			case "add", "edit":
				var g struct{ ID uint64 }
				if err := json.Unmarshal(value, &g); err != nil {
					a.err = err.Error()
					return
				}
				if request["op"] == "add" {
					a.settingsOpen = false
					a.query = ""
					a.revealSelection = true
				}
				if request["op"] == "add" || a.editOpen {
					a.selected = g.ID
				}
				a.editOpen = false
			case "remove":
				a.confirmOpen = false
				a.editOpen = false
			case "launch", "stop":
				var s session
				if err := json.Unmarshal(value, &s); err == nil {
					if i := slices.IndexFunc(a.sessions, func(existing session) bool { return existing.ID == s.ID }); i >= 0 {
						a.sessions[i] = s
					} else {
						a.sessions = append(a.sessions, s)
					}
				}
			case "log":
				var v struct {
					Text string `json:"text"`
				}
				_ = json.Unmarshal(value, &v)
				a.log = v.Text
				a.logOpen = true
				a.copiedAt = time.Time{}
			case "set_settings":
				var settings launchSettings
				if err := json.Unmarshal(value, &settings); err != nil {
					a.err = err.Error()
					return
				}
				a.defaults = &settings
				a.settingsDraft = form(settings)
				a.settingsCustomOutput = false
				a.settingsSaved = time.Now()
			case "set_lan":
				var lan lanStatus
				if err := json.Unmarshal(value, &lan); err != nil {
					a.err = err.Error()
					return
				}
				a.lan, a.lanError = &lan, ""
				a.lanPasswordOpen = false
				a.lanPassword = ""
			}
			if a.polling {
				a.refreshAgain = true
			} else {
				a.refresh()
			}
		})
	}()
}
func (a *app) current() *game {
	for i := range a.games {
		if a.games[i].ID == a.selected {
			return &a.games[i]
		}
	}
	return nil
}
func (a *app) session(id uint64) session {
	for _, s := range a.sessions {
		if s.ID == id {
			return s
		}
	}
	return session{}
}
func (a *app) active(id uint64) bool {
	s := a.session(id).State
	return s == "starting" || s == "running" || s == "stopping"
}
func stateName(state string) string {
	switch state {
	case "starting":
		return "正在启动"
	case "running":
		return "正在运行"
	case "stopping":
		return "正在停止"
	case "failed":
		return "启动失败"
	case "interrupted":
		return "会话中断"
	}
	return ""
}
func (a *app) shown() []game {
	query := strings.ToLower(strings.TrimSpace(a.query))
	var out []game
	for _, g := range a.games {
		if strings.Contains(strings.ToLower(g.Name), query) {
			out = append(out, g)
		}
	}
	return out
}

func (a *app) selectVisible() {
	shown := a.shown()
	for _, g := range shown {
		if g.ID == a.selected {
			return
		}
	}
	a.libraryScroll.Y = 0
	a.selected = 0
	if len(shown) > 0 {
		a.selected = shown[0].ID
	}
}

func (a *app) searchChanged() {
	a.libraryScroll.Y = 0
	a.selectVisible()
	a.revealSelection = true
}

func (a *app) clearSearch() {
	a.query = ""
	a.searchChanged()
	a.focusSearch = true
}

func (a *app) launchOrStop() {
	if a.settingsOpen || a.current() == nil || !a.connected || a.busy || a.session(a.selected).State == "stopping" {
		return
	}
	if a.active(a.selected) && !a.session(a.selected).canResume() {
		a.send(object{"op": "stop", "id": a.selected})
		return
	}
	a.launch(a.selected)
}

func (a *app) launch(id uint64) {
	if !a.connected || a.busy {
		return
	}
	if a.session(id).canResume() {
		a.send(object{"op": "resume_desktop", "id": id})
	} else if !a.active(id) {
		a.send(object{"op": "launch", "id": id})
	}
}

func jsonString(v any) string { value, _ := json.Marshal(v); return string(value) }
func (a *app) openEditor(mode editorMode) {
	if a.busy || !a.connected {
		return
	}
	a.draft = editor{}
	if mode != addGame {
		g := a.current()
		if g == nil {
			return
		}
		a.draft = editor{
			LaunchTab: mode == editQuality, ID: g.ID, Name: g.Name, Executable: g.Executable,
			Launch: form(g.launchSettings), Arguments: jsonString(g.Arguments), Prefix: g.Prefix,
		}
		a.draft.SavedOutput = a.draft.Launch.Output
		if g.Cover != nil {
			a.draft.Cover = *g.Cover
		}
	}
	a.editorVersion++
	a.editOpen = true
	a.refreshOutputDisplay()
	a.err = ""
}
func (a *app) pick(kind string) {
	if a.draft.Pending || a.busy {
		return
	}
	a.draft.Pending = true
	version := a.editorVersion
	options := mygo.OpenDialogOptions{
		Parent: a.win, Title: "选择启动文件",
		Filters: []mygo.FileFilter{{Name: "Windows 程序", Extensions: []string{"exe", "EXE"}}},
	}
	switch kind {
	case "cover":
		options.Title = "选择封面"
		options.Filters = []mygo.FileFilter{{Name: "封面", Extensions: []string{"png", "jpg", "jpeg", "webp", "ico"}}}
	case "directory":
		options.Title = "选择游戏目录"
		options.ButtonLabel = "选择此目录"
		options.Directory = true
		options.Filters = nil
		options.DefaultPath = a.draft.Directory
	}
	go func() {
		paths, err := mygo.Dialog.Open(options)
		a.win.Update(func() {
			if !a.editOpen || a.editorVersion != version {
				return
			}
			a.draft.Pending = false
			if err != nil {
				a.draft.Error = err.Error()
				return
			}
			if len(paths) == 0 {
				return
			}
			a.draft.Error = ""
			switch kind {
			case "directory":
				a.inspectDirectory(paths[0])
			case "cover":
				a.draft.Cover = paths[0]
				a.draft.CoverEdited = true
			default:
				a.draft.Executable = paths[0]
				if a.draft.ID == 0 && filepath.Dir(paths[0]) != a.draft.Directory {
					a.draft.Directory = filepath.Dir(paths[0])
					a.draft.Scan = nil
				}
			}
		})
	}()
}

func (a *app) findCovers() {
	title := strings.TrimSpace(a.draft.Name)
	if title == "" || a.draft.Pending || a.busy {
		return
	}
	a.draft.Error = ""
	a.draft.CoverSearch = title
	a.draft.CoverCandidates = nil
	a.draft.Pending = true
	version := a.editorVersion
	go func() {
		data, err := call(object{"op": "find_covers", "title": title})
		var covers []coverCandidate
		if err == nil {
			err = json.Unmarshal(data, &covers)
		}
		a.win.Update(func() {
			if !a.editOpen || a.editorVersion != version {
				return
			}
			a.draft.Pending = false
			if strings.TrimSpace(a.draft.Name) != title {
				a.draft.CoverSearch = ""
				return
			}
			if err != nil {
				a.draft.CoverSearch = ""
				a.draft.Error = err.Error()
				return
			}
			a.draft.CoverCandidates = covers
		})
	}()
}

func (a *app) chooseCover(candidate coverCandidate) {
	a.draft.Error = ""
	a.draft.Pending = true
	version := a.editorVersion
	go func() {
		data, err := call(object{"op": "download_cover", "url": candidate.URL})
		var path string
		if err == nil {
			err = json.Unmarshal(data, &path)
		}
		a.win.Update(func() {
			if !a.editOpen || a.editorVersion != version {
				return
			}
			a.draft.Pending = false
			if err != nil {
				a.draft.Error = err.Error()
				return
			}
			a.draft.Cover = path
			a.draft.CoverEdited = true
			a.draft.CoverSearch = ""
			a.draft.CoverCandidates = nil
		})
	}()
}

func (a *app) inspectDirectory(directory string) {
	directory = strings.TrimSpace(directory)
	if directory == "" || a.draft.Pending || a.busy {
		return
	}
	path, err := filepath.Abs(directory)
	if err != nil {
		a.draft.Error = err.Error()
		return
	}
	a.draft.Directory = path
	a.draft.Scan = nil
	a.draft.Executable = ""
	a.draft.Error = ""
	a.draft.Pending = true
	version := a.editorVersion
	go func() {
		data, err := call(object{"op": "inspect", "directory": path})
		var scan inspection
		if err == nil {
			err = json.Unmarshal(data, &scan)
		}
		a.win.Update(func() {
			if !a.editOpen || a.editorVersion != version {
				return
			}
			a.draft.Pending = false
			if err != nil {
				a.draft.Error = err.Error()
				return
			}
			a.draft.Directory = scan.Directory
			a.draft.Scan = &scan
			a.draft.Executable = scan.Selected
		})
	}()
}

func parseResolution(input string) (resolution, error) {
	input = strings.NewReplacer("×", "x", "X", "x").Replace(strings.TrimSpace(input))
	parts := strings.Split(input, "x")
	err := fmt.Errorf("分辨率请填写 1–16384 范围内的宽和高，例如 1280x720")
	if len(parts) != 2 {
		return resolution{}, err
	}
	w, e1 := strconv.Atoi(strings.TrimSpace(parts[0]))
	h, e2 := strconv.Atoi(strings.TrimSpace(parts[1]))
	if e1 != nil || e2 != nil || w < 1 || h < 1 || w > 16384 || h > 16384 {
		return resolution{}, err
	}
	return resolution{w, h}, nil
}

func form(s launchSettings) launchDraft {
	e := s.Enhancement
	d := launchDraft{Proton: s.Proton, Filter: e.Filter}
	if e.Enabled != nil {
		d.Enabled = strconv.FormatBool(*e.Enabled)
	}
	if e.Fullscreen != nil {
		d.Fullscreen = strconv.FormatBool(*e.Fullscreen)
	}
	if e.Source != nil {
		d.Source = fmt.Sprintf("%dx%d", e.Source.Width, e.Source.Height)
	}
	if e.Output != nil {
		d.Output = e.Output.String()
	}
	if e.FPS != nil {
		d.FPS = strconv.Itoa(*e.FPS)
	}
	if s.Environment != nil {
		d.Environment = jsonString(*s.Environment)
	}
	return d
}

func (d launchDraft) values(global bool) (launchSettings, error) {
	s := launchSettings{Proton: strings.TrimSpace(d.Proton)}
	if d.CustomProton && s.Proton == "" {
		return s, errors.New("请填写 Proton 路径")
	}
	e := &s.Enhancement
	e.Filter = d.Filter
	if d.Enabled != "" {
		value := d.Enabled == "true"
		e.Enabled = &value
	}
	if d.Fullscreen != "" {
		value := d.Fullscreen == "true"
		e.Fullscreen = &value
	}
	for _, field := range []struct {
		name, input string
		value       **resolution
	}{
		{"游戏分辨率", d.Source, &e.Source},
		{"输出分辨率", d.Output, &e.Output},
	} {
		if !global && strings.TrimSpace(field.input) == "" {
			continue
		}
		value, err := parseResolution(field.input)
		if err != nil {
			return s, fmt.Errorf("%s：%w", field.name, err)
		}
		*field.value = &value
	}
	if input := strings.TrimSpace(d.FPS); global || input != "" {
		value, err := strconv.Atoi(input)
		if err != nil || value < 1 || value > 360 {
			return s, errors.New("帧率应为 1–360 的整数")
		}
		e.FPS = &value
	}
	if input := strings.TrimSpace(d.Environment); input != "" {
		var env map[string]string
		if json.Unmarshal([]byte(input), &env) != nil || env == nil {
			return s, errors.New("环境变量应为 JSON 字符串键值对象")
		}
		s.Environment = &env
	}
	return s, nil
}

func (a *app) saveSettings() {
	if !a.connected || a.busy || a.defaults == nil || a.settingsDraft == form(*a.defaults) {
		return
	}
	settings, err := a.settingsDraft.values(true)
	if err != nil {
		a.err = err.Error()
		return
	}
	a.send(object{"op": "set_settings", "settings": settings})
}

func (a *app) save() {
	e := &a.draft
	if e.Pending || a.busy {
		return
	}
	exe := strings.TrimSpace(e.Executable)
	name := strings.TrimSpace(e.Name)
	if exe == "" {
		e.Error = "请选择游戏的 .exe 启动文件"
		return
	}
	if e.ID == 0 {
		request := object{"op": "add", "executable": exe}
		if name != "" {
			request["name"] = name
		}
		a.send(request)
		return
	}
	if name == "" {
		e.Error = "请填写游戏名称"
		return
	}
	overrides, err := e.Launch.values(false)
	if err != nil {
		e.Error = err.Error()
		return
	}
	var arguments []string
	if json.Unmarshal([]byte(e.Arguments), &arguments) != nil || arguments == nil {
		e.Error = "启动参数应为 JSON 字符串数组"
		return
	}
	changes := object{"name": name, "executable": exe, "overrides": overrides, "arguments": arguments}
	if e.Prefix != "" {
		changes["prefix"] = e.Prefix
	}
	if e.CoverEdited {
		if cover := strings.TrimSpace(e.Cover); cover != "" {
			changes["cover"] = cover
		} else {
			changes["clear_cover"] = true
		}
	}
	e.Error = ""
	a.send(object{"op": "edit", "id": e.ID, "changes": changes})
}

//go:embed assets/sakura.png
var sakuraArt []byte

//go:embed assets/foliage.png
var foliageArt []byte

func main() {
	if err := ui.RegisterFont(iconFont, iconFamily); err != nil {
		log.Fatalf("无法加载界面图标：%v", err)
	}
	sakura, err := ui.DecodeImage(sakuraArt)
	if err != nil {
		log.Fatal(err)
	}
	foliage, err := ui.DecodeImage(foliageArt)
	if err != nil {
		log.Fatal(err)
	}
	a := &app{
		appearance: loadAppearance(),
		images:     make(map[string]ui.ImageSource),
		sakura:     sakura,
		foliage:    foliage,
		focusShelf: true,
	}
	mygo.App.WhenReady(func() {
		a.win = mygo.NewWindow(mygo.WindowOptions{
			Title:             "GalScaler",
			Width:             1320,
			Height:            700,
			UseContentSize:    true,
			MinWidth:          840,
			MinHeight:         500,
			TitleBarStyle:     mygo.TitleBarHidden,
			TitleBarHeight:    toolbarHeight,
			HideWindowButtons: true,
			Content:           ui.View(a.view),
		})
		a.win.OnMove(a.refreshOutputDisplay)
		a.win.OnResize(a.refreshOutputDisplay)
		mygo.Screen.OnDisplaysChanged(a.refreshOutputDisplay)
		a.refresh()
		go func() {
			for range time.NewTicker(2500 * time.Millisecond).C {
				a.win.Update(a.refresh)
			}
		}()
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}
