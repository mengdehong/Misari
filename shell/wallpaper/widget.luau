local function render()
    local data = noctalia.state.get("wallpaper") or {}
    local output = barWidget.outputName()
    local view = data.snapshot and data.snapshot.outputs[output]
    local text = "未连接"
    if data.connected then
        text = view and (view.pending and "加载中" or view.effective_paused and "已暂停"
            or view.renderer == "running" and "运行中" or "未设置") or "未设置"
    end
    barWidget.setGlyph("photo")
    barWidget.setText("")
    barWidget.setTooltip("Wallpaper · " .. (output or "") .. " · " .. text)
end

function update()
    render()
end

function onClick()
    noctalia.togglePanel("misari/wallpaper:browser")
end

function onIpc(event)
    if event == "toggle" then onClick() end
end

noctalia.state.watch("wallpaper", render)
render()
