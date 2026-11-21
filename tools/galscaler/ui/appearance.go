package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

type palette struct {
	theme                                  *ui.Theme
	canvas, panel, raised, line            ui.Color
	heading, ink, muted, accent, selection ui.Color
	sky, actionFrom, actionTo              ui.Color
	shadow, live, gold, failure            ui.Color
}

const (
	controlRadius = 9
	panelRadius   = 16
	dialogRadius  = 20
)

var day = newPalette(false, palette{
	canvas: ui.Hex("#f8fbfe"), panel: ui.Hex("#ffffff"), raised: ui.Hex("#eff5fa"), line: ui.Hex("#e4edf5"),
	heading: ui.Hex("#23374a"), ink: ui.Hex("#23374a"), muted: ui.Hex("#576d80"),
	accent: ui.Hex("#366b98"), selection: ui.Hex("#477fab"),
	sky: ui.Hex("#e6f3fd"), actionFrom: ui.Hex("#3b75a5"), actionTo: ui.Hex("#3b6c9e"),
	shadow: ui.Hex("#172c41"), live: ui.Hex("#356b58"), gold: ui.Hex("#8b591b"), failure: ui.Hex("#a44554"),
})

var night = newPalette(true, palette{
	canvas: ui.Hex("#171f2b"), panel: ui.Hex("#1d2836"), raised: ui.Hex("#253448"), line: ui.Hex("#344457"),
	heading: ui.Hex("#eef5fb"), ink: ui.Hex("#eef5fb"), muted: ui.Hex("#afc0d1"),
	accent: ui.Hex("#a9cfee"), selection: ui.Hex("#9cccf3"),
	sky: ui.Hex("#294357"), actionFrom: ui.Hex("#316996"), actionTo: ui.Hex("#3a5d91"),
	shadow: ui.Hex("#0a111b"), live: ui.Hex("#9ecdb8"), gold: ui.Hex("#efbf83"), failure: ui.Hex("#ffadb7"),
})

func newPalette(dark bool, p palette) *palette {
	t := ui.LightTheme()
	if dark {
		t = ui.DarkTheme()
	}
	t = t.WithAccent(p.accent)
	t.Background = p.canvas
	t.Surface = p.raised
	t.SurfaceHover = p.raised.Mix(p.selection, .12)
	t.SurfacePressed = p.raised.Mix(p.selection, .23)
	t.Border = p.line
	t.Text = p.ink
	t.TextMuted = p.muted
	t.InactiveAccent = p.accent
	t.Selection = p.selection.Alpha(.30)
	t.Focus = p.selection
	t.Inverse = p.heading
	t.InverseText = p.panel
	t.Success = p.live
	t.Warning = p.gold
	t.Danger = p.failure
	t.Scrollbar = p.muted.Alpha(.36)
	t.ScrollbarWidth = 5
	t.Font = "Noto Sans CJK SC"
	t.FontSize = 14
	t.Radius = controlRadius
	p.theme = t
	return &p
}

func skin(c *ui.Context) *palette {
	if c.Theme().Dark {
		return night
	}
	return day
}

var forceReducedMotion = os.Getenv("GALSCALER_REDUCE_MOTION") == "1"

func reducedMotion(c *ui.Context) bool {
	return forceReducedMotion || c.Preferences().ReduceMotion
}

var quickSpring = ui.Animation{Spring: ui.SpringOptions{Duration: 280 * time.Millisecond, Bounce: .24}}
var starFade = ui.Animation{Duration: 420 * time.Millisecond, Ease: ui.CubicBezier(0, 0, 1, 1)}

func paintStar(p *ui.Painter, x, y, radius float32, color ui.Color) {
	cut := radius * .25
	path := new(ui.Path).MoveTo(x, y-radius).LineTo(x+cut, y-cut).LineTo(x+radius, y).
		LineTo(x+cut, y+cut).LineTo(x, y+radius).LineTo(x-cut, y+cut).
		LineTo(x-radius, y).LineTo(x-cut, y-cut).Close()
	p.FillPath(path, color)
}

func brandTile(c *ui.Context) {
	p := skin(c)
	tile := ui.Box(c).Size(25, 28).Center()
	var lift float32
	if !reducedMotion(c) {
		if tile.Hovered() {
			lift = -3
		}
		lift = tile.AnimateTo("drift", lift, quickSpring)
	}
	tile.DrawOver(func(draw *ui.Painter, r ui.Rect) {
		paintStar(draw, r.X+r.W-2, r.Y+4+lift, 3, p.selection)
	}).Children(func() {
		glyph(c, iGame).FontSize(21).TextColor(p.heading)
	})
}

func sparkleClick(c *ui.Context, b ui.Element) bool {
	clicked := b.Clicked()
	if reducedMotion(c) {
		return clicked
	}
	if clicked {
		b.Jump("stars", 1)
	}
	left := b.AnimateTo("stars", 0, starFade)
	if left > 0 {
		p := skin(c)
		travel := 1 - left*left
		b.DrawOver(func(draw *ui.Painter, r ui.Rect) {
			x, y := r.X+r.W-12, r.Y+7
			paintStar(draw, x+9*travel, y-21*travel, 5, p.selection.Alpha(left))
			paintStar(draw, x-13*travel, y-14*travel, 3, p.accent.Alpha(left))
			paintStar(draw, x+19*travel, y+5*travel, 2.5, p.selection.Alpha(left*.8))
		})
	}
	return clicked
}

const (
	minBlur, maxBlur   = 0, 80
	minShade, maxShade = 0, 100
	minZoom, maxZoom   = 1, 2
)

type backgroundSettings struct {
	Custom bool    `json:"custom"`
	Path   string  `json:"path,omitempty"`
	Blur   float64 `json:"blur"`
	Shade  float64 `json:"shade"`
	Zoom   float64 `json:"zoom"`
}

type appearance struct {
	backgroundSettings
	image               *ui.Bitmap
	imageErr, configErr string
	picking, dirty      bool
}

func appearancePath() (string, error) {
	dir, err := os.UserConfigDir()
	return filepath.Join(dir, "galscaler", "appearance.json"), err
}

func loadAppearance() appearance {
	defaults := backgroundSettings{Blur: 8, Shade: 65, Zoom: 1}
	a := appearance{backgroundSettings: defaults}
	path, err := appearancePath()
	if err != nil {
		a.configErr = fmt.Sprintf("无法读取外观设置：%v", err)
		return a
	}
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return a
	}
	settings := defaults
	if err == nil {
		err = json.Unmarshal(data, &settings)
	}
	if err != nil {
		a.configErr = fmt.Sprintf("无法读取外观设置：%v", err)
		return a
	}
	settings.Blur = min(maxBlur, max(minBlur, settings.Blur))
	settings.Shade = min(maxShade, max(minShade, settings.Shade))
	settings.Zoom = min(maxZoom, max(minZoom, settings.Zoom))
	a.backgroundSettings = settings
	if a.Path != "" {
		a.image, err = readBackground(a.Path)
		if err != nil {
			a.imageErr = fmt.Sprintf("无法读取背景图片：%v", err)
		}
	}
	return a
}

func (a *appearance) save() {
	a.dirty = false
	a.configErr = ""
	if err := a.writeSettings(); err != nil {
		a.configErr = fmt.Sprintf("无法保存外观设置：%v", err)
	}
}

func (a *appearance) writeSettings() error {
	path, err := appearancePath()
	if err != nil {
		return err
	}
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	f, err := os.CreateTemp(dir, ".appearance-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	err = json.NewEncoder(f).Encode(a.backgroundSettings)
	if err := errors.Join(err, f.Close()); err != nil {
		return err
	}
	return os.Rename(f.Name(), path)
}

func readBackground(path string) (*ui.Bitmap, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	return ui.DecodeBitmapScaled(data, 4096)
}

func (a *appearance) pick(win *mygo.Window) {
	a.picking = true
	options := mygo.OpenDialogOptions{
		Parent: win, Title: "选择背景图片", DefaultPath: a.Path,
		Filters: []mygo.FileFilter{{Name: "图片", Extensions: []string{"png", "jpg", "jpeg", "webp", "bmp", "gif"}}},
	}
	go func() {
		paths, err := mygo.Dialog.Open(options)
		var picture *ui.Bitmap
		if err == nil && len(paths) > 0 {
			picture, err = readBackground(paths[0])
		}
		win.Update(func() {
			a.picking = false
			switch {
			case err != nil:
				a.imageErr = fmt.Sprintf("无法打开背景图片：%v", err)
			case picture != nil:
				a.Path, a.image, a.imageErr = paths[0], picture, ""
				a.save()
			}
		})
	}()
}

func (a *appearance) active() bool { return a.Custom && a.image != nil }

func (a *appearance) backdrop(c *ui.Context) {
	blur, zoom := float32(a.Blur), float32(a.Zoom)
	margin := 3 * blur
	image := a.image
	ui.Box(c.Key("background")).Absolute().Fill().Clip().ZIndex(-1).PassThrough().AccessHidden(true).Children(func() {
		ui.Box(c).Absolute().Left(-margin).Right(-margin).Top(-margin).Bottom(-margin).Clip().Blur(blur).Draw(func(p *ui.Painter, r ui.Rect) {
			paintBackground(p, r, image, margin, zoom)
		})
		ui.Box(c).Absolute().Fill().Background(skin(c).canvas.Alpha(float32(a.Shade / 100)))
	})
}

func paintBackground(p *ui.Painter, r ui.Rect, image *ui.Bitmap, margin, zoom float32) {
	iw, ih := ui.ImageSize(image)
	scale := max((r.W-2*margin)/iw, (r.H-2*margin)/ih) * zoom
	w, h := iw*scale, ih*scale
	x, y := r.X+(r.W-w)/2, r.Y+(r.H-h)/2
	// Extend the image's edge pixels into the blur margin to keep its framing stable.
	dstX, dstY := [4]float32{r.X, x, x + w, r.X + r.W}, [4]float32{r.Y, y, y + h, r.Y + r.H}
	srcX, srcY := [3]float32{0, 0, iw - 1}, [3]float32{0, 0, ih - 1}
	srcW, srcH := [3]float32{1, iw, 1}, [3]float32{1, ih, 1}
	for row := range 3 {
		for col := range 3 {
			src := ui.Rect{X: srcX[col], Y: srcY[row], W: srcW[col], H: srcH[row]}
			dst := ui.Rect{X: dstX[col], Y: dstY[row], W: dstX[col+1] - dstX[col], H: dstY[row+1] - dstY[row]}
			p.ImageRegion(image, src, dst, ui.ImageOptions{})
		}
	}
}

func (a *appearance) view(c *ui.Context, win *mygo.Window) {
	p := skin(c)
	before := a.backgroundSettings
	pressed := false
	settingsGroup(c).MaxWidth(720).Children(func() {
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			settingsHeading(c, iImage, "应用背景")
		})
		rule(c)
		const standard, custom = "默认背景", "自定义图片"
		mode := standard
		if a.Custom {
			mode = custom
		}
		ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
			ui.Text(c, "背景来源").Grow(1)
			if ui.Select(c.Key("background-mode"), &mode, []string{standard, custom}).
				Label("背景来源").Width(225).Height(36).Disabled(a.picking).Changed() {
				a.Custom = mode == custom
			}
		})
		if a.Custom {
			rule(c)
			ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
				ui.Column(c).Grow(1).MinWidth(0).Gap(2).Children(func() {
					ui.Text(c, "背景图片")
					if a.Path != "" {
						small(c, filepath.Base(a.Path)).MaxLines(1).Tooltip(a.Path)
					}
				})
				if a.image != nil {
					ui.Image(c, a.image).Size(86, 48).Fit(ui.Cover).Radius(6).Clip().AccessHidden(true)
				}
				if actionButton(c.Key("pick-background"), iImage, "选择图片", false).Height(34).Disabled(a.picking).Clicked() {
					a.pick(win)
				}
				if a.Path != "" && iconButton(c.Key("clear-background"), iClose, "移除背景图片").
					Size(34, 34).Disabled(a.picking).Clicked() {
					a.Path, a.image, a.imageErr = "", nil, ""
				}
			})
			if a.image != nil {
				for _, row := range []struct {
					label, format string
					value         *float64
					lo, hi, step  float64
				}{
					{"背景模糊", "%.0f px", &a.Blur, minBlur, maxBlur, 1},
					{"遮罩浓度", "%.0f%%", &a.Shade, minShade, maxShade, 1},
					{"缩放", "%.2f×", &a.Zoom, minZoom, maxZoom, .05},
				} {
					ui.Row(c.Key(row.label)).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
						ui.Text(c, row.label).Grow(1)
						slider := ui.Slider(c, row.value, row.lo, row.hi).Step(row.step).Label(row.label).Width(159)
						pressed = slider.Pressed() || pressed
						small(c, fmt.Sprintf(row.format, *row.value)).Width(56).TextAlign(ui.End)
					})
				}
			}
			if a.imageErr != "" {
				small(c, a.imageErr).TextColor(p.failure).FillWidth()
			}
		}
		if a.configErr != "" {
			small(c, a.configErr).TextColor(p.failure).FillWidth()
		}
	})
	a.dirty = a.dirty || before != a.backgroundSettings
	if a.dirty && !pressed {
		a.save()
	}
}
