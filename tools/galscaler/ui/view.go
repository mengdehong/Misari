package main

import (
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/egoist/mygo"
	"github.com/egoist/mygo/ui"
)

var filters = []string{"cunny", "anime4k", "linear"}
var filterNames = []string{"CuNNy", "Anime4K", "无滤镜"}

func small(c *ui.Context, s string) ui.Element {
	return ui.Text(c, s).FontSize(12).TextColor(skin(c).muted)
}
func rule(c *ui.Context) { ui.Box(c).FillWidth().Height(1).Background(skin(c).line) }
func stateColor(c *ui.Context, state string) ui.Color {
	p := skin(c)
	switch state {
	case "running":
		return p.live
	case "starting":
		return p.accent
	case "failed", "interrupted":
		return p.failure
	}
	return p.muted
}
func badge(c *ui.Context, label string, color ui.Color) {
	ui.Row(c).Padding(5, 9).Gap(6).Radius(9).Background(color.Alpha(.11)).AlignItems(ui.Center).Children(func() {
		ui.Text(c, "●").FontSize(6).TextColor(color)
		ui.Text(c, label).FontSize(12).FontWeight(600).TextColor(color).NoWrap()
	})
}
func (a *app) view(c *ui.Context) {
	p := skin(c)
	c.SetTheme(p.theme)
	if forceReducedMotion {
		c.DisableAnimations()
	}
	if !a.editOpen && !a.confirmOpen && !a.logOpen && !a.lanPasswordOpen {
		if c.Shortcut(ui.Ctrl, ui.KeyN) && a.connected {
			a.openEditor(addGame)
		}
		if c.Shortcut(ui.Ctrl, ui.KeyF) {
			a.settingsOpen = false
			a.focusSearch = true
		}
		if c.Shortcut(ui.Ctrl, ui.KeyE) {
			if !a.settingsOpen {
				a.openEditor(editGame)
			}
		}
		if c.Shortcut(ui.Ctrl, ui.KeyEnter) {
			if a.settingsOpen {
				if a.settingsTab == 0 {
					a.saveSettings()
				}
			} else {
				a.launchOrStop()
			}
		}
		if c.Shortcut(0, ui.KeyEscape) && a.query != "" {
			a.clearSearch()
		}
		if c.Shortcut(0, ui.KeyF5) {
			a.reload()
		}
	}
	w, h := c.Size()
	ui.Column(c).Fill().Background(p.canvas).Children(func() {
		if a.appearance.active() {
			a.appearance.backdrop(c)
		}
		a.toolbar(c, w)
		rule(c)
		a.library(c, w, h-toolbarHeight-1)
	})
	if !a.settingsOpen && a.appearance.dirty {
		a.appearance.save()
	}
	a.editorView(c, h)
	a.lanPasswordView(c.Key("lan-password"))
	dialog(c, &a.confirmOpen, func() {
		ui.Text(c, "移出游戏库？").FontSize(21).FontWeight(600).TextColor(p.heading)
		ui.Text(c, "游戏文件和存档会保留，你可以随时重新添加。")
		if a.err != "" {
			small(c, a.err).TextColor(p.failure)
		}
		ui.Row(c).Gap(10).Children(func() {
			ui.Spacer(c)
			if iconButton(c, iClose, "取消").Disabled(a.busy).Clicked() {
				a.confirmOpen = false
			}
			if iconButton(c, iTrash, "确认移出游戏库").TextColor(p.failure).Disabled(a.busy).Clicked() {
				a.send(object{"op": "remove", "id": a.draft.ID})
			}
		})
	}).Key("remove").Width(440)
	dialog(c, &a.logOpen, func() {
		ui.Row(c).FillWidth().Gap(6).AlignItems(ui.Center).Children(func() {
			ui.Text(c, "启动日志").FontSize(21).FontWeight(600).TextColor(p.heading).Grow(1)
			code, label := iCopy, "复制日志"
			if time.Since(a.copiedAt) < 3*time.Second {
				code, label = iCheck, "已复制"
			}
			if iconButton(c, code, label).Disabled(a.log == "").Clicked() {
				mygo.Clipboard.WriteText(a.log)
				a.copiedAt = time.Now()
			}
			if iconButton(c, iClose, "关闭 · Esc").Clicked() {
				a.logOpen = false
			}
		})
		value := a.log
		if value == "" {
			value = "暂无日志内容"
		}
		ui.TextArea(c, &value).ReadOnly(true).Height(min(h-180, 520)).FontSize(12).Font("monospace")
	}).Key("logs").Width(850).MaxWidth(w - 80)
}
func (a *app) art(g *game) ui.ImageSource {
	if g == nil || g.Cover == nil {
		return nil
	}
	path := *g.Cover
	src, known := a.images[path]
	if !known {
		if data, err := os.ReadFile(path); err == nil {
			src, _ = ui.DecodeImage(data)
		}
		a.images[path] = src
	}
	return src
}
func dialog(c *ui.Context, open *bool, content func()) ui.Element {
	p := skin(c)
	return ui.DialogBase(c, open, func(backdrop, body ui.Element) {
		backdrop.Background(p.shadow.Alpha(.48))
		body.Padding(22).Gap(12).Background(p.panel).Border(1, p.line).Radius(dialogRadius).
			Shadow(0, 8, 28, 0, p.shadow.Alpha(.17)).Transition(ui.ElementTransition{
			Duration: 180 * time.Millisecond, ExitDuration: 120 * time.Millisecond,
			Enter: &ui.Motion{Y: 8, Scale: .98}, Exit: &ui.Motion{Y: 4},
		})
		content()
	})
}

func colorMotion() ui.ElementTransition {
	return ui.ElementTransition{Colors: true, Duration: 140 * time.Millisecond}
}

func menuPanel(c *ui.Context) func(ui.Element) {
	p := skin(c)
	return func(box ui.Element) {
		box.Background(p.panel).Border(1, p.line).Radius(controlRadius).NoShadow().Shadow(0, 6, 20, 0, p.shadow.Alpha(.18)).
			Transition(ui.ElementTransition{
				Colors: true, Duration: 150 * time.Millisecond, Enter: &ui.Motion{Y: -4, Flip: true},
			})
	}
}

const toolbarHeight = 56

func (a *app) toolbar(c *ui.Context, width float32) {
	p := skin(c)
	bar := ui.Row(c).FillWidth().Height(toolbarHeight).Padding(0, 18).Gap(8).AlignItems(ui.Center)
	if a.appearance.active() {
		bar.Background(p.panel.Alpha(.82))
	} else {
		bar.Background(p.panel)
	}
	bar.Children(func() {
		ui.Row(c).FillHeight().Gap(10).AlignItems(ui.Center).DragWindow().Children(func() {
			brandTile(c)
			ui.Text(c, "GalScaler").FontSize(18).FontWeight(700).TextColor(p.heading).NoWrap()
		})
		ui.Box(c).Grow(1).FillHeight().DragWindow()
		if a.settingsOpen {
			if actionButton(c.Key("global-settings"), iPrev, "返回游戏库", false).Height(34).Clicked() {
				a.settingsOpen = false
			}
		} else {
			a.searchBox(c, min(244, width*.22))
			add := quietButton(c, actionButton(c.Key("add"), iAdd, "添加游戏", false)).
				Height(34).Padding(0, 8).Tooltip("Ctrl N").Disabled(!a.connected || a.busy)
			if sparkleClick(c, add) {
				a.openEditor(addGame)
			}
			if iconButton(c.Key("global-settings"), iGear, "全局设置").Size(34, 34).Clicked() {
				a.settingsOpen = true
				a.reload()
			}
		}
		ui.Box(c).Size(1, 18).Background(p.line)
		if iconButton(c.Key("minimize"), iMinus, "最小化").Size(32, 32).Clicked() {
			a.win.Minimize()
		}
		code, label := iStop, "最大化"
		maximized := a.win.IsMaximized()
		if maximized {
			code, label = iRestore, "还原窗口"
		}
		if iconButton(c.Key("maximize"), code, label).Size(32, 32).Clicked() {
			if maximized {
				a.win.Unmaximize()
			} else {
				a.win.Maximize()
			}
		}
		if iconButton(c.Key("close"), iClose, "关闭 GalScaler").Size(32, 32).Clicked() {
			a.win.Close()
		}
	})
}

const (
	densityAuto = iota
	densityGallery
	densityStandard
	densityCompact
)

var densityLabels = [...]string{"自动", "大", "标准", "紧凑"}

func (a *app) coverDensity() int {
	if a.density != densityAuto {
		return a.density
	}
	switch {
	case len(a.games) <= 5:
		return densityGallery
	case len(a.games) <= 20:
		return densityStandard
	default:
		return densityCompact
	}
}

func (a *app) library(c *ui.Context, width, height float32) {
	p := skin(c)
	padding := min(36, width*.032)
	density := a.coverDensity()
	controls := len(a.games) > 20 || a.density != densityAuto || a.listView
	custom := a.appearance.active()
	gallery := density == densityGallery && !a.listView && !a.settingsOpen && !custom
	top, gap := min(48, width*.039), float32(18)
	if a.settingsOpen {
		top, gap = 14, 12
	}
	library := ui.Box(c).FillWidth().Grow(1).MinHeight(0)
	if !custom {
		library.Draw(func(draw *ui.Painter, r ui.Rect) {
			draw.FillRadialGradient(r, ui.RadialGradient{
				Center: ui.Point{X: .99, Y: .01}, Radius: min(width*.66, height*.78),
				Stops: []ui.GradientStop{{Color: p.sky, At: 0}, {Color: p.sky.Alpha(0), At: 1}},
			}, 0)
			if !a.settingsOpen {
				paintStar(draw, r.X+r.W*.06, r.Y+r.H*.58, 4, p.selection.Alpha(.24))
				paintStar(draw, r.X+r.W*.94, r.Y+r.H*.26, 5, p.accent.Alpha(.22))
				paintStar(draw, r.X+r.W*.96, r.Y+r.H*.31, 3, p.selection.Alpha(.28))
			}
		})
	}
	if !a.settingsOpen {
		library.TabStop(false)
	}
	library.Children(func() {
		if gallery {
			artSize := min(400, width*.31, height*.58)
			opacity := float32(.34)
			if p.theme.Dark {
				opacity = .26
			}
			ui.Image(c, a.sakura).Absolute().Top(0).Left(-artSize*.08).Size(artSize, artSize).ZIndex(-1).
				Fit(ui.Contain).Opacity(opacity).AccessHidden(true).PassThrough()
			foliageSize := artSize * .83
			ui.Image(c, a.foliage).Absolute().Top(min(92, height*.11)).Right(0).Size(foliageSize, foliageSize).ZIndex(-1).
				Fit(ui.Contain).Opacity(opacity * .7).AccessHidden(true).PassThrough()
		}
		ui.Column(c).Fill().MaxWidth(1640).AlignSelf(ui.Center).
			Padding(top, padding, 16, padding).Gap(gap).Children(func() {
			available := height - top - 16
			if a.connectionError != "" && (len(a.games) > 0 || a.settingsOpen) {
				a.notice(c.Key("connection"), "暂时连不上 GalScaler。", a.connectionError, iRefresh, "重试", a.polling, a.reload)
				available -= 44 + gap
			}
			if a.err != "" && !a.editOpen && !a.lanPasswordOpen {
				a.notice(c.Key("error"), a.err, a.err, iClose, "关闭提示", false, func() { a.err = "" })
				available -= 44 + gap
			}
			if a.settingsOpen {
				a.settingsView(c, width-2*padding)
				return
			}
			shown := a.shown()
			width = min(width, 1640) - 2*padding
			a.shelfHeader(c, len(shown), width, controls)
			if len(shown) == 0 {
				a.emptyLibrary(c)
				return
			}

			detailWidth := min(350, max(218, width*.27))
			gutter := min(60, width*.047)
			shelfWidth := width - detailWidth - gutter
			rows, coverHeight := a.shelfRows(shown, shelfWidth-2*shelfInset, available-38-gap, density, a.listView)
			var usedWidth float32
			for _, row := range rows {
				usedWidth = max(usedWidth, row.width)
			}
			ui.Row(c).FillWidth().Grow(1).MinHeight(0).Gap(gutter).AlignItems(ui.Stretch).Children(func() {
				a.shelf(c, library, rows, coverHeight, usedWidth+2*shelfInset, density, a.listView)
				if current := a.current(); current != nil {
					a.work(c, current, detailWidth)
				}
			})
		})
	})
}

func (a *app) shelfHeader(c *ui.Context, count int, width float32, controls bool) {
	p := skin(c)
	ui.Row(c).Width(width).Height(38).AlignSelf(ui.Center).AlignItems(ui.Center).Gap(10).Children(func() {
		ui.Text(c, "游戏库").FontSize(27).FontWeight(650).TextColor(p.heading).NoWrap()
		if !controls {
			return
		}
		small(c, fmt.Sprintf("%d 部作品", count)).NoWrap()
		ui.Spacer(c)
		for _, option := range []struct {
			list  bool
			icon  icon
			label string
		}{{false, iGrid, "封面网格"}, {true, iList, "列表"}} {
			active := a.listView == option.list
			b := iconButton(c.Key(option.label), option.icon, option.label).Size(34, 32).AccessSelected(active)
			if active {
				b.Background(p.accent.Alpha(.13)).TextColor(p.accent)
			}
			if a.appearance.active() {
				fill := p.panel.Alpha(.85)
				if active {
					fill = p.panel.Mix(p.accent, .13)
				}
				b.Background(fill)
			}
			if b.Clicked() && !active {
				a.listView = option.list
				a.libraryScroll.Y = 0
				a.revealSelection = true
			}
		}
		if !a.listView {
			label := "封面大小 · " + densityLabels[a.density]
			if a.density == densityAuto {
				label = "封面大小 · " + densityLabels[a.coverDensity()] + "（自动）"
			}
			actionButton(c.Key("density"), iDown, label, false).Height(32).Border(1, p.line).
				PopoverMenuButton(func(m *ui.Menu) {
					for i, label := range densityLabels {
						if m.Item(label).Radio(a.density == i).Chosen() {
							a.density = i
							a.libraryScroll.Y = 0
							a.revealSelection = true
						}
					}
				}, ui.PopoverMenuButtonOptions{Panel: menuPanel(c)})
		}
	})
}

func (a *app) notice(c *ui.Context, message, detail string, code icon, label string, disabled bool, action func()) {
	p := skin(c)
	fill := p.failure.Alpha(.07)
	if a.appearance.active() {
		fill = p.panel.Mix(p.failure, .07)
	}
	ui.Row(c).FillWidth().Height(44).Padding(0, 12).Gap(10).Radius(controlRadius).
		Background(fill).AlignItems(ui.Center).Children(func() {
		glyph(c, iWarning).FontSize(18).TextColor(p.failure)
		ui.Text(c, message).TextColor(p.failure).Grow(1).MinWidth(0).MaxLines(2).Tooltip(detail)
		if iconButton(c, code, label).Disabled(disabled).Clicked() {
			action()
		}
	})
}

func (a *app) searchBox(c *ui.Context, width float32) {
	p := skin(c)
	box := ui.Row(c).Width(width).Height(34).Padding(0, 10).Gap(8).Radius(controlRadius).Background(p.panel.Alpha(.8)).Border(1, p.line).AlignItems(ui.Center)
	if box.FocusWithin() {
		box.Border(1, p.accent)
	}
	box.Transition(colorMotion()).Children(func() {
		glyph(c, iSearch).FontSize(18).TextColor(p.muted)
		input := ui.TextInputBase(c.Key("search"), &a.query).Label("搜索游戏").Placeholder("搜索游戏").Grow(1).MinWidth(0).Height(32).OnTextKey(func(k ui.TextKey) bool {
			if k.Key != ui.KeyEscape || k.Mods != 0 || a.query == "" {
				return false
			}
			k.SetText("")
			a.clearSearch()
			return true
		})
		if a.focusSearch {
			input.Focus()
			a.focusSearch = false
		}
		if input.Changed() {
			a.searchChanged()
		}
		if a.query != "" && iconButton(c, iClose, "清除搜索 · Esc").Size(28, 28).Clicked() {
			a.clearSearch()
		}
	})
}

func (a *app) emptyLibrary(c *ui.Context) {
	p := skin(c)
	ui.Box(c).Grow(1).FillWidth().Center().Children(func() {
		ui.Column(c).Gap(12).AlignItems(ui.Center).Children(func() {
			code, title := iLibrary, "游戏库还是空的"
			switch {
			case a.connectionError != "" && len(a.games) == 0:
				code, title = iDisconnected, "暂时连不上 GalScaler"
			case !a.connected && len(a.games) == 0:
				code, title = iLoading, "正在打开游戏库…"
			case strings.TrimSpace(a.query) != "":
				code, title = iSearch, "没有找到匹配的游戏"
			}
			ui.Box(c).Size(58, 58).Radius(18).Gradient(p.raised, p.sky, 135).Center().Children(func() {
				glyph(c, code).FontSize(28).TextColor(p.accent)
			})
			ui.Text(c, title).FontSize(20).FontWeight(600).TextColor(p.heading).Tooltip(a.connectionError)
			switch code {
			case iDisconnected:
				if actionButton(c, iRefresh, "重试", false).Disabled(a.polling).Clicked() {
					a.reload()
				}
			case iSearch:
				if actionButton(c, iClose, "清除搜索", false).Clicked() {
					a.clearSearch()
				}
			}
		})
	})
}

const (
	shelfGap   = 16
	shelfInset = 5
)

type shelfCard struct {
	game
	width, center float32
}

type shelfRow struct {
	cards []shelfCard
	width float32
}

func (a *app) shelfRows(games []game, width, height float32, density int, list bool) ([]shelfRow, float32) {
	target := [...]float32{0, 220, 156, 116}[density]
	gallery := density == densityGallery && !list
	columns := max(1, int((width+shelfGap)/(target+shelfGap)))
	if gallery {
		columns = max(3, columns)
	}
	cell := min(target+28, (width-float32(columns-1)*shelfGap)/float32(columns))
	coverHeight := cell * 1.5
	if gallery {
		coverHeight = min(coverHeight, max(105, height-78))
	} else if density == densityCompact {
		coverHeight = min(coverHeight, max(96, (height-96)/2))
	}
	cell = min(cell, coverHeight/1.5)
	if list {
		columns, cell, coverHeight = 1, width, 46
	}
	var rows []shelfRow
	used := columns
	for _, g := range games {
		span := 1
		if gallery {
			if w, h := ui.ImageSize(a.art(&g)); h > 0 && w/h >= 1.35 {
				span = 2
			}
		}
		if used+span > columns {
			rows = append(rows, shelfRow{})
			used = 0
		}
		row := &rows[len(rows)-1]
		cardWidth := float32(span)*cell + float32(span-1)*shelfGap
		if len(row.cards) > 0 {
			row.width += shelfGap
		}
		row.cards = append(row.cards, shelfCard{game: g, width: cardWidth, center: row.width + cardWidth/2})
		row.width += cardWidth
		used += span
	}
	return rows, coverHeight
}

func (a *app) shelf(c *ui.Context, library ui.Element, rows []shelfRow, coverHeight, width float32, density int, list bool) {
	scroll := ui.Scroll(c.Key("covers")).TrackScroll(&a.libraryScroll).Width(width).Shrink(0).MinHeight(0).
		Label("游戏封面").Role(ui.RoleListBox)
	if !a.editOpen && !a.confirmOpen && !a.logOpen {
		shortcut := func(key ui.Key) bool {
			return scroll.Shortcut(0, key) || library.Focused() && library.Shortcut(0, key)
		}
		for _, key := range []ui.Key{ui.KeyLeft, ui.KeyRight, ui.KeyUp, ui.KeyDown} {
			if shortcut(key) {
				if id := shelfNeighbor(rows, a.selected, key); id != a.selected {
					a.selected = id
					a.focusShelf = true
				}
			}
		}
		if shortcut(ui.KeyEnter) && !c.ShortcutRepeated() {
			a.launch(a.selected)
		}
	}
	gap := float32(shelfGap)
	if list {
		gap = 6
	}
	scroll.Children(func() {
		ui.Column(c).FillWidth().Padding(shelfInset, shelfInset, 12, shelfInset).Gap(gap).Children(func() {
			for _, row := range rows {
				ui.Row(c.Key(row.cards[0].ID)).FillWidth().Gap(shelfGap).AlignItems(ui.Start).Children(func() {
					for _, card := range row.cards {
						a.tile(c.Key(card.ID), card, coverHeight, density, list)
					}
				})
			}
		})
	})
}

func shelfNeighbor(rows []shelfRow, id uint64, key ui.Key) uint64 {
	for i, row := range rows {
		for j, card := range row.cards {
			if card.ID != id {
				continue
			}
			switch key {
			case ui.KeyLeft:
				if j > 0 {
					return row.cards[j-1].ID
				}
			case ui.KeyRight:
				if j+1 < len(row.cards) {
					return row.cards[j+1].ID
				}
			case ui.KeyUp, ui.KeyDown:
				dest := i + 1
				if key == ui.KeyUp {
					dest = i - 1
				}
				if dest < 0 || dest >= len(rows) {
					break
				}
				next := rows[dest]
				x := card.center
				for k, candidate := range next.cards {
					if k+1 == len(next.cards) || x <= (candidate.center+next.cards[k+1].center)/2 {
						return candidate.ID
					}
				}
			}
			return id
		}
	}
	return id
}

func (a *app) tile(c *ui.Context, card shelfCard, coverHeight float32, density int, list bool) {
	p := skin(c)
	g := &card.game
	tile := ui.ButtonBase(c).Width(card.width).Shrink(0).Padding(0).AlignItems(ui.Start).
		Label(g.Name).FocusRing(true).Role(ui.RoleOption)
	if sparkleClick(c, tile) {
		a.selected = g.ID
	}
	if tile.DoubleClicked() {
		a.selected = g.ID
		a.launch(g.ID)
	}
	tile.PopoverMenu(func(m *ui.Menu) { a.gameMenu(c, m, g) }, ui.PopoverMenuOptions{Panel: menuPanel(c)})
	selected := a.selected == g.ID
	tile.AccessSelected(selected).TabStop(selected)
	if selected && a.focusShelf {
		tile.Focus()
		a.focusShelf = false
	}
	if selected && a.revealSelection {
		tile.ScrollIntoView()
		a.revealSelection = false
	}

	state := a.session(g.ID).State
	hovered := tile.Hovered()
	tile.Children(func() {
		if list {
			row := ui.Row(c).FillWidth().Padding(4, 10, 4, 6).Gap(10).Radius(6).AlignItems(ui.Center)
			if hovered || selected {
				row.Background(p.raised)
			}
			row.Children(func() {
				if selected {
					ui.Box(c).Absolute().Left(0).Top(12).Bottom(12).Width(2).Radius(1).Background(p.selection)
				}
				ui.Box(c).Width(36).Children(func() { a.cover(c, g, coverHeight) })
				ui.Text(c, g.Name).FontWeight(600).TextColor(p.heading).Grow(1).MinWidth(0).MaxLines(1)
				if label := stateName(state); label != "" {
					badge(c, label, stateColor(c, state))
				}
			})
			return
		}
		ui.Column(c).FillWidth().Gap(4).Children(func() {
			poster := ui.Box(c).FillWidth()
			if !reducedMotion(c) {
				var lift float32
				if tile.Pressed() {
					lift = 1
				} else if hovered {
					lift = -2
				}
				poster.Translate(0, poster.AnimateTo("lift", lift, quickSpring))
			}
			poster.Children(func() { a.cover(c, g, coverHeight) })
			mark := ui.Box(c).Width(20).Height(2).Radius(1)
			if selected {
				mark.Background(p.selection)
			}
			size, lines := float32(14), 2
			if density == densityCompact {
				size, lines = 12, 1
			}
			ui.Column(c).FillWidth().MinHeight(size * 1.3 * float32(lines)).Gap(2).Children(func() {
				ui.Text(c, g.Name).FontSize(size).FontWeight(600).TextColor(p.heading).FillWidth().
					LineHeight(1.3).MaxLines(lines).Tooltip(g.Name)
				if label := stateName(state); label != "" {
					small(c, label).TextColor(stateColor(c, state)).MaxLines(1)
				}
			})
		})
	})
}

func (a *app) work(c *ui.Context, g *game, width float32) {
	p := skin(c)
	ui.Scroll(c.Key("current-work")).Width(width).Shrink(0).MinHeight(0).Children(func() {
		body := ui.Column(c.Key(g.ID)).FillWidth().Padding(shelfInset, 0, 6, 0).Gap(16).
			Transition(ui.ElementTransition{Duration: 100 * time.Millisecond, Enter: &ui.Motion{}})
		if a.appearance.active() {
			body.Padding(12).Radius(panelRadius).Background(p.panel.Alpha(.94))
		}
		body.Children(func() {
			session := a.session(g.ID)
			title, subtitle, _ := strings.Cut(g.Name, " - ")
			font := "Noto Serif"
			if strings.IndexFunc(title, func(r rune) bool {
				return unicode.Is(unicode.Han, r) || unicode.Is(unicode.Hiragana, r) || unicode.Is(unicode.Katakana, r)
			}) >= 0 {
				font = "Noto Serif CJK JP"
			}
			ui.Column(c).FillWidth().Gap(6).Children(func() {
				ui.Text(c, title).Font(font).FontSize(min(32, width*.109)).FontWeight(500).TextColor(p.heading).
					FillWidth().LineHeight(1.22).Balance().MaxLines(3).Tooltip(g.Name)
				if subtitle != "" {
					ui.Text(c, subtitle).Font(font).FontSize(16).TextColor(p.muted).
						FillWidth().MaxLines(2).Tooltip(g.Name)
				}
				small(c, a.pictureSummary(g)).MaxLines(1)
			})
			a.workActions(c, g, session)
			if session.Error != "" {
				small(c, session.Error).TextColor(p.failure).FillWidth().MaxLines(2).Tooltip(session.Error)
			}
		})
	})
}

func (a *app) pictureSummary(g *game) string {
	picture := g.Enhancement
	if a.defaults != nil {
		if picture.Enabled == nil {
			picture.Enabled = a.defaults.Enhancement.Enabled
		}
		if picture.Filter == "" {
			picture.Filter = a.defaults.Enhancement.Filter
		}
	}
	if picture.Enabled != nil && !*picture.Enabled {
		return "原始画质"
	}
	if i := slices.Index(filters, picture.Filter); i >= 0 {
		label, _, _ := strings.Cut(filterNames[i], " · ")
		return "画质增强 · " + label
	}
	return "画质增强"
}

func (a *app) workActions(c *ui.Context, g *game, session session) {
	disabled := !a.connected || a.busy
	if a.active(g.ID) && !session.canResume() {
		ui.Text(c, stateName(session.State)).FontSize(13).FontWeight(600).TextColor(stateColor(c, session.State))
	} else {
		label, code := "开始游戏", iPlay
		if session.canResume() {
			label = "在电脑上继续"
		} else if session.State == "failed" || session.State == "interrupted" {
			label, code = "重新启动", iRefresh
		}
		start := actionButton(c.Key("play"), code, label, true).Height(40).MinWidth(120).
			AlignSelf(ui.Start).Disabled(disabled)
		if sparkleClick(c, start) {
			a.launch(g.ID)
		}
	}
	ui.Row(c).FillWidth().AlignItems(ui.Center).Gap(4).Children(func() {
		picture := quietButton(c, ui.Button(c.Key("picture"), "画质设置")).Height(34).Padding(0, 5).FontSize(12)
		if pressButton(c, picture).Disabled(disabled).Clicked() {
			a.openEditor(editQuality)
		}
		settings := quietButton(c, ui.Button(c.Key("settings"), "游戏设置")).Height(34).Padding(0, 5).FontSize(12)
		if pressButton(c, settings).Tooltip("Ctrl E").Disabled(disabled).Clicked() {
			a.openEditor(editGame)
		}
		iconButton(c.Key("more"), iMore, "更多操作").Size(32, 34).
			PopoverMenuButton(func(m *ui.Menu) { a.gameMenu(c, m, g) }, ui.PopoverMenuButtonOptions{Panel: menuPanel(c)})
	})
}

func (a *app) gameMenu(c *ui.Context, m *ui.Menu, g *game) {
	session := a.session(g.ID)
	state := session.State
	disabled := !a.connected || a.busy
	switch state {
	case "starting", "running":
		if session.canResume() && m.Item("在电脑上继续").Disabled(disabled).Chosen() {
			a.launch(g.ID)
		}
		label := "停止游戏"
		if state == "starting" {
			label = "取消启动"
		}
		if m.Item(label).Disabled(disabled).Chosen() {
			a.send(object{"op": "stop", "id": g.ID})
		}
	case "stopping":
		m.Item("正在停止…").Disabled(true)
	default:
		if m.Item("开始游戏").Disabled(disabled).Chosen() {
			a.selected = g.ID
			a.launch(g.ID)
		}
	}
	m.Separator()
	if m.Item("画质").Disabled(disabled).Chosen() {
		a.selected = g.ID
		a.openEditor(editQuality)
	}
	if m.Item("游戏设置").Disabled(disabled).Chosen() {
		a.selected = g.ID
		a.openEditor(editGame)
	}
	if m.Item("打开游戏目录").Disabled(g.Executable == "").Chosen() {
		path := url.URL{Scheme: "file", Path: filepath.Dir(g.Executable)}
		c.OpenURLThen(path.String(), func(err error) {
			if err != nil {
				a.err = err.Error()
			}
		})
	}
	if state != "" && m.Item("启动日志").Disabled(disabled).Chosen() {
		a.send(object{"op": "log", "id": g.ID, "lines": 150})
	}
}

func (a *app) cover(c *ui.Context, g *game, height float32) {
	p := skin(c)
	box := ui.Box(c).FillWidth().Height(height).Radius(min(controlRadius, height*.13)).Border(1, p.line).Clip().Center()
	if src := a.art(g); src != nil {
		box.Background(p.canvas).Children(func() {
			ui.Image(c, src).Fill().Fit(ui.Contain)
		})
		return
	}
	box.Gradient(p.raised, p.sky, 135).Children(func() {
		glyph(c, iImage).FontSize(min(23, height*.44)).TextColor(p.accent)
	})
}

func field(c *ui.Context, key, label, hint string, value *string) {
	ui.Column(c).Grow(1).MinWidth(0).Gap(6).Children(func() {
		small(c, label)
		input := ui.TextInput(c.Key(key), value).Label(label).FillWidth().Height(40).Placeholder(hint)
		if key == "name" && hint == "" {
			input.AutoFocus()
		}
	})
}

func disclosure(c *ui.Context, label string, open *bool) bool {
	code := iNext
	if *open {
		code = iDown
	}
	button := pressButton(c, ui.Button(c, "")).Height(36).Padding(0, 12).Gap(7).
		AlignSelf(ui.Start).Label(label).AccessSelected(*open).Children(func() {
		glyph(c, code).FontSize(17)
		ui.Text(c, label).FontWeight(550).NoWrap()
	})
	if button.Clicked() {
		*open = !*open
	}
	return *open
}

func (a *app) settingsView(c *ui.Context, width float32) {
	p := skin(c)
	ui.Column(c).FillWidth().MaxWidth(1060).Grow(1).MinHeight(0).AlignSelf(ui.Center).Gap(14).Children(func() {
		header := ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Padding(4, 2)
		if a.appearance.active() {
			header.Background(p.panel.Alpha(.94)).Radius(panelRadius).Padding(8, 12)
		}
		header.Children(func() {
			ui.Text(c, "全局设置").FontSize(27).FontWeight(700).TextColor(p.heading).Grow(1)
			if ui.Segmented(c.Key("settings-tab"), &a.settingsTab, "游戏启动", "界面外观", "局域网连接").
				Label("设置类别").Width(264).Changed() && a.settingsTab == 2 {
				if a.polling {
					a.refreshAgain = true
				} else {
					a.refresh()
				}
			}
		})
		ui.Scroll(c.Key(a.settingsTab)).Grow(1).MinHeight(0).FillWidth().Children(func() {
			ui.Column(c).FillWidth().Padding(2, 8, 8, 2).Gap(14).Children(func() {
				if a.settingsTab == 1 {
					a.appearance.view(c.Key("appearance"), a.win)
					return
				}
				if a.settingsTab == 2 {
					a.lanView(c.Key("lan"))
					return
				}
				if a.defaults == nil {
					if a.polling && a.connectionError == "" {
						small(c, "读取中…")
					}
					return
				}
				if width < 940 {
					a.runtimeSettings(c.Key("runtime-group"))
					a.pictureSettings(c.Key("picture-group"))
					a.advancedSettings(c.Key("advanced-group"))
					return
				}
				ui.Row(c).FillWidth().AlignItems(ui.Start).Gap(14).Children(func() {
					ui.Column(c).Grow(1).MinWidth(0).Children(func() {
						a.pictureSettings(c.Key("picture-group"))
					})
					ui.Column(c).Width(322).Shrink(0).Gap(14).Children(func() {
						a.runtimeSettings(c.Key("runtime-group"))
						a.advancedSettings(c.Key("advanced-group"))
					})
				})
			})
		})
		if a.settingsTab == 0 && a.defaults != nil {
			a.settingsActions(c.Key("actions"))
		}
	})
}

func (a *app) lanView(c *ui.Context) {
	p := skin(c)
	if a.lanError != "" {
		small(c, a.lanError).TextColor(p.failure)
		return
	}
	if a.lan == nil {
		if a.connected {
			small(c, "读取中…")
		}
		return
	}
	lan := a.lan
	enabled := lan.Enabled
	disabled := !a.connected || a.busy
	settingsGroup(c).MaxWidth(720).Children(func() {
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			settingsHeading(c, iNetwork, "允许局域网连接")
			if ui.Switch(c.Key("enabled"), &enabled).Label("允许局域网连接").Disabled(disabled).Changed() {
				if enabled && !lan.HasPassword {
					a.openLanPassword(true)
				} else {
					a.send(object{"op": "set_lan", "enabled": enabled})
				}
			}
		})
		if lan.Error != "" {
			small(c, lan.Error).TextColor(p.failure)
		}
		if lan.Name != "" {
			rule(c)
			ui.Row(c).FillWidth().MinHeight(36).Gap(10).AlignItems(ui.Center).Children(func() {
				ui.Text(c, "设备名称").Grow(1)
				ui.Text(c, lan.Name).FontWeight(600).TextColor(p.heading).MaxWidth(400)
			})
		}
		rule(c)
		ui.Row(c).FillWidth().MinHeight(36).Gap(10).AlignItems(ui.Center).Children(func() {
			ui.Text(c, "连接地址").Grow(1)
			if lan.Address == "" || !a.connected {
				small(c, "开启后显示")
				return
			}
			code := iCopy
			if time.Since(a.lanCopied) < 3*time.Second {
				code = iCheck
			}
			if actionButton(c.Key("address"), code, lan.Address, false).Height(36).
				Label("复制连接地址 " + lan.Address).Tooltip("复制连接地址").Clicked() {
				mygo.Clipboard.WriteText(lan.Address)
				a.lanCopied = time.Now()
			}
		})
		rule(c)
		ui.Row(c).FillWidth().MinHeight(36).Gap(10).AlignItems(ui.Center).Children(func() {
			ui.Text(c, "连接密码").Grow(1)
			label := "设置密码"
			if lan.HasPassword {
				label = "修改密码"
			}
			if pressButton(c, ui.Button(c.Key("password"), label)).Height(36).Disabled(disabled).Clicked() {
				a.openLanPassword(lan.Enabled)
			}
		})
	})
}

func (a *app) openLanPassword(enable bool) {
	a.err = ""
	a.lanPassword = ""
	a.lanEnable = enable
	a.lanPasswordOpen = true
}

func (a *app) lanPasswordView(c *ui.Context) {
	p := skin(c)
	if !a.lanPasswordOpen {
		a.lanPassword = ""
	}
	wasOpen := a.lanPasswordOpen
	dialog(c, &a.lanPasswordOpen, func() {
		ui.Text(c, "设置连接密码").FontSize(21).FontWeight(600).TextColor(p.heading)
		input := ui.TextInput(c.Key("password"), &a.lanPassword).Label("连接密码").
			Placeholder("6–64 个字符").Password().AutoFocus().FillWidth().Height(40).Disabled(a.busy)
		if a.err != "" {
			small(c, a.err).TextColor(p.failure)
		}
		ui.Row(c).FillWidth().Gap(8).Children(func() {
			ui.Spacer(c)
			if pressButton(c, ui.Button(c.Key("cancel"), "取消")).Height(36).Disabled(a.busy).Clicked() {
				a.lanPasswordOpen = false
			}
			label := "保存密码"
			if a.lanEnable && a.lan != nil && !a.lan.Enabled {
				label = "保存并开启"
			}
			length := len([]rune(a.lanPassword))
			valid := length >= 6 && length <= 64 && strings.TrimSpace(a.lanPassword) != ""
			save := actionButton(c.Key("save"), iCheck, label, true).Height(36).
				Disabled(!valid || a.busy || !a.connected)
			saved := sparkleClick(c, save)
			if valid && a.connected && (saved || input.Submitted()) {
				a.send(object{"op": "set_lan", "enabled": a.lanEnable, "password": a.lanPassword})
			}
		})
	}).Width(420)
	if wasOpen && !a.lanPasswordOpen && !a.busy {
		a.err = ""
	}
}

func settingsGroup(c *ui.Context) ui.Element {
	p := skin(c)
	return ui.Column(c).FillWidth().Padding(16, 20).Gap(14).
		Background(p.panel.Alpha(.94)).Border(1, p.line).Radius(panelRadius)
}

func settingsHeading(c *ui.Context, code icon, title string) {
	p := skin(c)
	glyph(c, code).FontSize(19).TextColor(p.accent)
	ui.Text(c, title).FontSize(17).FontWeight(650).TextColor(p.heading).Grow(1)
}

func (a *app) pictureSettings(c *ui.Context) {
	d := &a.settingsDraft
	enabled := d.Enabled == "true"
	settingsGroup(c).Children(func() {
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			settingsHeading(c, iSliders, "画质增强")
			if ui.Switch(c.Key("enhancement-enabled"), &enabled).Label("画质增强").Disabled(a.busy).Changed() {
				d.Enabled = strconv.FormatBool(enabled)
			}
		})
		rule(c)
		ui.Column(c).FillWidth().Gap(14).Disabled(!enabled || a.busy).Children(func() {
			a.pictureFields(c, d, nil)
		})
	})
}

func (a *app) runtimeSettings(c *ui.Context) {
	p := skin(c)
	d := a.diagnostics
	var problems []string
	if d != nil {
		if d.Wayland == "" && a.settingsDraft.Enabled == "true" {
			problems = append(problems, "画质增强需要 Wayland。")
		}
		if d.UmuRun == "" {
			problems = append(problems, "请安装 umu-run。")
		}
		if !d.SystemdUser {
			problems = append(problems, "systemd 用户会话不可用。")
		}
		if len(d.Runtimes) == 0 && a.defaults.Proton == "" {
			problems = append(problems, "游戏启动需要 Proton。")
		}
	}
	settingsGroup(c).Children(func() {
		ui.Row(c).FillWidth().Gap(8).AlignItems(ui.Center).Children(func() {
			settingsHeading(c, iGame, "运行环境")
			switch {
			case a.checking:
				small(c, "检查中…")
			case a.connected && d != nil && len(problems) == 0:
				small(c, "环境正常").TextColor(p.live)
			case len(problems) != 0:
				small(c, "需检查").TextColor(p.gold)
			}
			if iconButton(c.Key("check-now"), iRefresh, "重新检查 · F5").Size(30, 30).
				Disabled(!a.connected || a.checking).Clicked() {
				a.reload()
			}
		})
		rule(c)
		a.managedProton(c.Key("ge-proton"))
		for _, problem := range problems {
			ui.Row(c).FillWidth().Gap(7).AlignItems(ui.Center).Children(func() {
				glyph(c, iWarning).FontSize(15).TextColor(p.gold)
				small(c, problem).TextColor(p.gold).Grow(1).MinWidth(0)
			})
		}
	})
}

func (a *app) managedProton(c *ui.Context) {
	p := skin(c)
	var installed proton
	for _, runtime := range a.runtimes {
		if strings.HasPrefix(runtime.Name, "GE-Proton") {
			installed = runtime
			break
		}
	}
	label, status := "检查更新", "已安装 · "+installed.Name
	install := installed.Path == ""
	if install {
		label, status = "安装 GE-Proton", "尚未安装"
	}
	if a.geLatest != nil {
		if *a.geLatest == "" && installed.Path != "" {
			status = "已是最新版 · " + installed.Name
		} else if *a.geLatest != "" {
			label, status, install = "安装新版", "可用版本 · "+*a.geLatest, true
		}
	}
	d := a.diagnostics
	available := d != nil && d.GeTools
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		ui.Text(c, "GE-Proton").FontWeight(600).TextColor(p.heading)
		if a.geAction != "" {
			status = a.geAction
		}
		small(c, status)
		if a.geAction == "" && d != nil {
			defaultPath := a.defaults.Proton
			if defaultPath == "" {
				defaultPath = d.AutomaticProton
			}
			if defaultPath != "" && (defaultPath != installed.Path || a.geLatest != nil && *a.geLatest != "") {
				small(c, "默认使用 · "+filepath.Base(defaultPath))
			}
		}
		if d != nil && !available {
			small(c, "下载和安装需要 curl 与 tar。").TextColor(p.gold)
		}
		if a.geError != "" {
			small(c, a.geError).TextColor(p.failure)
		}
		code := iRefresh
		if install {
			code = iAdd
		}
		if actionButton(c.Key("action"), code, label, install).Height(36).AlignSelf(ui.Start).
			Disabled(!a.connected || !available || a.geAction != "").Clicked() {
			a.manageProton(install)
		}
	})
}

func (a *app) advancedSettings(c *ui.Context) {
	settingsGroup(c).Children(func() {
		ui.Collapsible(c.Key("advanced"), "高级选项", &a.settingsAdvanced, func() {
			ui.Column(c).FillWidth().Gap(6).Padding(8, 0, 0, 0).Children(func() {
				a.protonField(c.Key("runtime"), &a.settingsDraft, nil, false)
				small(c, "环境变量（JSON）")
				ui.TextArea(c.Key("environment"), &a.settingsDraft.Environment).Label("环境变量（JSON）").
					FillWidth().Height(112).Placeholder(`{"LC_ALL":"ja_JP.UTF-8"}`).Disabled(a.busy)
			})
		}).FillWidth()
	})
}

func (a *app) settingsActions(c *ui.Context) {
	p := skin(c)
	dirty := a.settingsDraft != form(*a.defaults)
	ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Padding(8, 12).
		Background(p.panel.Alpha(.94)).Border(1, p.line).Radius(panelRadius).Children(func() {
		if dirty {
			small(c, "未保存").TextColor(p.gold)
		} else if time.Since(a.settingsSaved) < 5*time.Second {
			glyph(c, iCheck).FontSize(16).TextColor(p.live)
			small(c, "已保存").TextColor(p.live).Tooltip("下次启动游戏时生效")
		}
		ui.Spacer(c)
		if dirty && pressButton(c, ui.Button(c.Key("discard"), "放弃修改")).Height(36).Disabled(a.busy).Clicked() {
			a.settingsDraft = form(*a.defaults)
			a.settingsCustomOutput = false
			a.err = ""
		}
		label := "保存"
		if a.busy {
			label = "正在保存…"
		}
		save := actionButton(c.Key("save-settings"), iCheck, label, true).Height(36).
			Tooltip("Ctrl Enter").Disabled(!dirty || !a.connected || a.busy)
		if sparkleClick(c, save) {
			a.saveSettings()
		}
	})
}

func (a *app) protonField(c *ui.Context, d *launchDraft, defaults *launchDraft, shared bool) {
	automatic := "自动选择 Proton"
	label := automatic
	if shared && a.sharedProton != "" {
		label = "共享环境 · " + filepath.Base(a.sharedProton)
	} else if defaults != nil {
		inherited := automatic
		if defaults.Proton != "" {
			inherited = filepath.Base(defaults.Proton)
		}
		label = "使用全局 · " + inherited
	}
	const custom = "自定义路径…"
	options := []string{label}
	names := map[string]int{}
	for _, r := range a.runtimes {
		names[r.Name]++
	}
	selected := label
	if d.CustomProton || d.Proton != "" {
		selected = custom
	}
	for _, r := range a.runtimes {
		name := r.Name
		if names[name] > 1 {
			name = r.Path
		}
		options = append(options, name)
		if !d.CustomProton && d.Proton == r.Path {
			selected = name
		}
	}
	options = append(options, custom)
	small(c, "Proton 兼容层")
	if ui.Select(c.Key("runtime"), &selected, options).Label("Proton 兼容层").FillWidth().Height(40).Changed() {
		d.CustomProton = selected == custom
		if selected == label {
			d.Proton = ""
		} else if index := slices.Index(options, selected) - 1; index >= 0 && index < len(a.runtimes) {
			d.Proton = a.runtimes[index].Path
		}
	}
	if selected == custom {
		field(c, "proton", "Proton 路径", "", &d.Proton)
	}
}

func settingSelect(c *ui.Context, key, label string, value *string, values, labels []string, inherited string) {
	if inherited != "" {
		values = append([]string{""}, values...)
		labels = append([]string{"使用全局 · " + inherited}, labels...)
	}
	selected := labels[0]
	if i := slices.Index(values, *value); i >= 0 {
		selected = labels[i]
	}
	ui.Column(c).Grow(1).MinWidth(0).Gap(6).Children(func() {
		small(c, label)
		if ui.Select(c.Key(key), &selected, labels).Label(label).FillWidth().Height(40).Changed() {
			if i := slices.Index(labels, selected); i >= 0 {
				*value = values[i]
			}
		}
	})
}

func (a *app) launchFields(c *ui.Context, d, defaults *launchDraft, shared bool) {
	a.protonField(c, d, defaults, shared)
	rule(c)
	a.pictureFields(c, d, defaults)
}

func (a *app) pictureFields(c *ui.Context, d, defaults *launchDraft) {
	boolean := []string{"true", "false"}
	fullscreen := []string{"全屏", "窗口"}
	inherited := func(value string, values, labels []string) string {
		if i := slices.Index(values, value); i >= 0 && defaults != nil {
			return labels[i]
		}
		return ""
	}
	var base launchDraft
	if defaults != nil {
		base = *defaults
	}
	hint := func(value string) string {
		if value == "" {
			return ""
		}
		return "全局 · " + value
	}
	if defaults != nil {
		enabled := []string{"开启", "关闭"}
		settingSelect(c, "enabled", "画质增强", &d.Enabled, boolean, enabled, inherited(base.Enabled, boolean, enabled))
	}
	ui.Row(c).FillWidth().Gap(12).Children(func() {
		settingSelect(c, "filter", "增强算法", &d.Filter, filters, filterNames, inherited(base.Filter, filters, filterNames))
		settingSelect(c, "fullscreen", "窗口模式", &d.Fullscreen, boolean, fullscreen, inherited(base.Fullscreen, boolean, fullscreen))
	})
	ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Start).Children(func() {
		field(c, "source", "游戏分辨率", hint(base.Source), &d.Source)
		a.outputField(c, d, defaults)
	})
	if defaults != nil {
		ui.Row(c).FillWidth().Gap(12).Children(func() {
			field(c, "fps", "帧率", hint(base.FPS), &d.FPS)
			ui.Spacer(c)
		})
		return
	}
	rule(c)
	ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
		small(c, "帧率").NoWrap()
		fps, _ := strconv.Atoi(d.FPS)
		value := float64(max(1, min(360, fps)))
		slider := ui.Slider(c.Key("fps-slider"), &value, 1, 360).Step(1).Label("帧率").Grow(1)
		input := ui.NumberInput(c.Key("fps"), &value, 1, 360, 1).
			Label("帧率（FPS）").Width(118).Height(36)
		if slider.Changed() || input.Changed() {
			d.FPS = strconv.Itoa(int(value + .5))
		}
		small(c, "FPS").NoWrap()
	})
}

func (a *app) directoryField(c *ui.Context) {
	e := &a.draft
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		small(c, "游戏目录")
		ui.Row(c).FillWidth().AlignItems(ui.Center).Gap(8).Children(func() {
			disabled := a.busy || e.Pending
			input := ui.TextInput(c.Key("directory"), &e.Directory).Label("游戏目录").
				Placeholder("选择目录，或粘贴路径后查找").Height(40).Grow(1).MinWidth(0).Disabled(disabled).AutoFocus()
			if input.Changed() {
				e.Scan = nil
				e.Executable = ""
				e.Error = ""
			}
			if iconButton(c.Key("browse-directory"), iFolder, "浏览游戏目录").Disabled(disabled).Clicked() {
				a.pick("directory")
			}
			if iconButton(c.Key("scan"), iScan, "查找启动文件").Disabled(disabled || strings.TrimSpace(e.Directory) == "").Clicked() || input.Submitted() {
				a.inspectDirectory(e.Directory)
			}
		})
		if e.Pending {
			small(c, "正在查找启动文件…")
		} else if scan := e.Scan; scan != nil && len(scan.Candidates) == 0 && e.Executable == "" {
			small(c, "没有找到启动文件，可以在下方手动选择。")
		}
	})
}

func (a *app) entryField(c *ui.Context) {
	e := &a.draft
	disabled := a.busy || e.Pending
	ui.Row(c).FillWidth().AlignItems(ui.End).Gap(8).Children(func() {
		ui.Column(c).Grow(1).MinWidth(0).Gap(6).Children(func() {
			small(c, "启动文件")
			if e.Scan != nil && len(e.Scan.Candidates) > 1 {
				var options []string
				selected := filepath.Base(e.Executable)
				if e.Executable == "" {
					selected = "选择要运行的 .exe"
				}
				for _, candidate := range e.Scan.Candidates {
					options = append(options, filepath.Base(candidate))
				}
				if ui.Select(c.Key("candidate"), &selected, options).Label("启动文件").FillWidth().Height(40).Disabled(disabled).Changed() {
					for _, candidate := range e.Scan.Candidates {
						if filepath.Base(candidate) == selected {
							e.Executable = candidate
							e.Error = ""
							break
						}
					}
				}
				return
			}
			if ui.TextInput(c.Key("executable"), &e.Executable).Label("启动文件").
				Placeholder("选择或粘贴 .exe 文件路径").FillWidth().Height(40).Disabled(disabled).Changed() {
				e.Error = ""
			}
		})
		if iconButton(c, iFolder, "手动选择 .exe").Disabled(disabled).Clicked() {
			a.pick("executable")
		}
	})
}

func (a *app) editorView(c *ui.Context, height float32) {
	p := skin(c)
	d := &a.draft
	edit := d.ID != 0
	canSave := func() bool {
		return a.connected && !a.busy && !a.draft.Pending && strings.TrimSpace(a.draft.Executable) != ""
	}
	h := float32(430)
	if edit {
		h = 480
	}
	if d.LaunchTab || d.CoverCandidates != nil {
		h = min(height-48, 728)
	}
	dialog(c, &a.editOpen, func() {
		if c.Shortcut(ui.Ctrl, ui.KeyEnter) && canSave() && !a.confirmOpen {
			a.save()
		}
		title := "添加游戏"
		if edit {
			title = "游戏设置"
		}
		ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
			ui.Text(c, title).FontSize(23).FontWeight(600).TextColor(p.heading).Grow(1)
			if iconButton(c, iClose, "关闭 · Esc").Disabled(a.busy).Clicked() {
				a.editOpen = false
			}
		})
		if edit {
			ui.Row(c).FillWidth().Gap(6).Children(func() {
				for i, label := range []string{"基本信息", "启动与画面"} {
					selected := d.LaunchTab == (i == 1)
					button := pressButton(c, ui.Button(c.Key(label), label)).Height(36).AccessSelected(selected)
					if selected {
						button.Background(p.accent.Alpha(.14)).TextColor(p.accent)
					}
					if button.Clicked() {
						d.LaunchTab = i == 1
					}
				}
			})
		}
		ui.Scroll(c.Key(d.LaunchTab)).Grow(1).MinHeight(0).FillWidth().Children(func() {
			ui.Column(c).FillWidth().Padding(2, 10, 4, 2).Gap(12).Children(func() {
				if edit && d.LaunchTab {
					if d.Launch != (launchDraft{}) {
						if pressButton(c, ui.Button(c.Key("reset-launch"), "恢复全局设置")).
							Height(36).AlignSelf(ui.End).Disabled(a.busy).Clicked() {
							d.Launch = launchDraft{}
							d.CustomOutput = false
							d.Error = ""
							a.err = ""
						}
					}
					settingSelect(c, "prefix", "兼容环境", &d.Prefix,
						[]string{"shared", "independent"}, []string{"共享环境", "独立环境"}, "")
					if g := a.current(); g != nil && g.ID == d.ID && d.Prefix != g.Prefix {
						small(c, "存档保留在原环境").TextColor(p.gold)
					}
					base := form(*a.defaults)
					a.launchFields(c, &d.Launch, &base, d.Prefix == "shared")
					if disclosure(c.Key("expert"), "高级选项", &d.Expert) {
						field(c, "arguments", "启动参数（JSON 数组）", "", &d.Arguments)
						field(c, "environment", "环境变量（JSON）", "留空使用全局，填写 {} 清空", &d.Launch.Environment)
					}
					return
				}
				if !edit {
					a.directoryField(c)
				}
				a.entryField(c)
				if edit {
					field(c, "name", "游戏名称", "", &a.draft.Name)
					a.coverField(c)
				} else {
					field(c, "name", "游戏名称（可选）", "默认使用文件夹名称", &a.draft.Name)
				}
			})
		})
		errorText := a.draft.Error
		if errorText == "" {
			errorText = a.err
		}
		if !a.connected {
			errorText = "暂时连不上 GalScaler，请稍后再试。"
		}
		if errorText != "" {
			small(c, errorText).TextColor(p.failure).FillWidth().MaxLines(2).Tooltip(errorText)
		}
		rule(c)
		ui.Row(c).FillWidth().Gap(10).AlignItems(ui.Center).Children(func() {
			if edit {
				if iconButton(c, iTrash, "移出游戏库").TextColor(p.failure).
					Disabled(!a.connected || a.busy || a.active(a.draft.ID)).Clicked() {
					a.err = ""
					a.confirmOpen = true
				}
				if a.active(a.draft.ID) {
					small(c, "下次启动生效")
				}
			}
			ui.Spacer(c)
			label := "添加到游戏库"
			if edit {
				label = "保存设置"
			}
			if a.busy && !a.confirmOpen {
				label = "正在保存…"
			}
			save := pressButton(c, primaryButton(c, label)).Height(40).Tooltip("Ctrl Enter").Disabled(!canSave())
			if sparkleClick(c, save) {
				a.save()
			}
		})
	}).Width(600).Height(h)
}

func (a *app) coverField(c *ui.Context) {
	p := skin(c)
	e := &a.draft
	disabled := !a.connected || a.busy || e.Pending
	title := strings.TrimSpace(e.Name)
	ui.Column(c).FillWidth().Gap(6).Children(func() {
		small(c, "封面")
		ui.Row(c).FillWidth().AlignItems(ui.Center).Gap(8).Children(func() {
			if e.Cover != "" {
				if src := a.art(&game{Cover: &e.Cover}); src != nil {
					ui.Image(c, src).Size(56, 40).Fit(ui.Contain).Radius(8)
				}
			}
			if ui.TextInput(c.Key("cover"), &e.Cover).Label("封面").
				Placeholder("选择图片，或清空以移除封面").
				Height(40).Grow(1).MinWidth(0).Disabled(disabled).Changed() {
				e.CoverEdited = true
			}
			if iconButton(c, iFolder, "选择封面文件").Disabled(disabled).Clicked() {
				a.pick("cover")
			}
			code, label := iSearch, "查找封面"
			if e.Pending && e.CoverSearch == title && e.CoverCandidates == nil {
				code, label = iLoading, "正在查找…"
			}
			if iconButton(c.Key("find-cover"), code, label).Disabled(disabled || title == "").Clicked() {
				a.findCovers()
			}
		})
		if e.CoverCandidates == nil || title != e.CoverSearch {
			return
		}
		if len(e.CoverCandidates) == 0 {
			small(c, "VNDB 没有找到可用封面")
		}
		for _, candidate := range e.CoverCandidates {
			row := ui.ButtonBase(c.Key(candidate.URL)).FillWidth().Padding(6, 10).Radius(8).
				Background(p.raised).Label(candidate.Title).FocusRing(true).Disabled(disabled)
			row.Children(func() {
				ui.Row(c).FillWidth().Gap(12).AlignItems(ui.Center).Children(func() {
					ui.Box(c).Size(42, 54).Background(p.panel).Radius(5).Clip().Center().Children(func() {
						if src := a.art(&game{Cover: candidate.Thumbnail}); src != nil {
							ui.Image(c, src).Fill().Fit(ui.Contain)
						} else {
							glyph(c, iImage)
						}
					})
					ui.Column(c).Grow(1).MinWidth(0).Gap(3).Children(func() {
						ui.Text(c, candidate.Title).MaxLines(2)
						detail := "作品"
						if candidate.Release {
							detail = "发行版"
						}
						if candidate.Released != nil && len(*candidate.Released) >= 4 {
							detail += " · " + (*candidate.Released)[:4]
						}
						small(c, detail)
					})
				})
			})
			if row.Clicked() {
				a.chooseCover(candidate)
			}
		}
	})
}
