local data = noctalia.state.get("wallpaper") or {
    assets = {}, snapshot = { outputs = {} }, operations = {}, connected = false,
}
local opened = false
local screen, selected, target = nil, nil, nil
local page, query = "library", ""
local collection, addingTo = "library", nil
local listForm, deletingList, handledCreation = nil, nil, nil
local importPath, importRevision, listRevision = "", 0, 0
local removingSource = nil
local propertyDrafts, propertyRevisions, rotationDrafts = {}, {}, {}
local titleEdit = nil
local pickerError = nil
local handledOperations, handledLibrary = {}, nil
local kindFilter, sourceFilter, sortOrder, gridPage = 0, 0, 0, 1
local viewMode, detailTab = "grid", 0
local settingsTab = 0
local fit, transition = "cover", "fade"
local requestId, searchRevision = 0, 0
local focusSearch = false
local pendingRender = false
local sliderValues = {}
local render
local indexedAssets, indexedLibrary = nil, nil
local assetIndex, assetNames, assetSearch = {}, {}, {}
local hiddenIds, favoriteIds, memberPositions = {}, {}, {}
local indexedTitles = {}
local collectionCache, filterCache = nil, nil
local fits = { "cover", "contain", "stretch" }
local transitions = { "fade", "disc", "honeycomb", "spiral", "stripes" }
local reasons = {
    user = "手动暂停", session_locked = "锁屏", session_inactive = "会话不活跃",
    sleep = "休眠", fullscreen = "全屏应用", frame_throttled = "帧提交节流",
    playback_error = "播放错误", other_audio = "其他应用正在播放音频",
}

local function request(action, output, extra)
    requestId += 1
    local message = table.clone(extra or {})
    message.id = tostring(noctalia.nowMs()) .. ":" .. tostring(requestId)
    message.action, message.output = action, output
    noctalia.state.set("wallpaper-request", message)
end

local function outputNames()
    local names = {}
    for name in pairs(data.snapshot.outputs) do table.insert(names, name) end
    table.sort(names)
    return names
end

local function originalName(asset)
    return asset.project and asset.name or (asset.name:gsub("%.[^.]+$", ""))
end

local function displayName(asset)
    local titles = data.library and data.library.titles
    return titles and titles[asset.id] or originalName(asset)
end

local function indexName(asset)
    local name = displayName(asset)
    assetNames[asset.id] = name:lower()
    assetSearch[asset.id] = (name .. " " .. asset.name .. " " .. asset.path):lower()
end

local function indexAssets()
    -- State is copied between VMs; revisions keep unrelated snapshots from
    -- invalidating the catalog and its derived lists.
    local revision = data.assetsRevision or data.assets
    if indexedAssets == revision then return end
    indexedAssets = revision
    assetIndex, assetNames, assetSearch = {}, {}, {}
    for _, asset in ipairs(data.assets) do
        assetIndex[asset.id] = asset
        indexName(asset)
    end
    collectionCache, filterCache = nil, nil
end

local function findAsset(id)
    indexAssets()
    return assetIndex[id]
end

local function scheduleRender()
    if not opened then return end
    pendingRender = true
    panel.setNeedsFrameTick(true)
end

local function assetName(id)
    local titles = data.library and data.library.titles
    if id and titles and titles[id] then return titles[id] end
    local asset = findAsset(id)
    if asset then return displayName(asset) end
    return id and (id:match("([^/]+)$") or id) or "未设置壁纸"
end

local function dynamic(asset)
    return asset.kind ~= "image"
end

local function source(asset)
    return asset.project ~= nil or asset.id:sub(1, 3) == "we:"
end

local function typeText(asset)
    return (dynamic(asset) and "动态" or "静态") .. " · " .. (source(asset) and "Wallpaper Engine" or "本地")
end

local function reasonText(values)
    local labels = {}
    for _, reason in ipairs(values or {}) do table.insert(labels, reasons[reason] or reason) end
    return table.concat(labels, "、")
end

local function status(view)
    if not data.connected then return "未连接", "error" end
    if not view then return "无显示器", "on_surface_variant" end
    if view.pending then return "加载中", "primary" end
    if view.renderer ~= "running" then return view.error and "播放异常" or view.current and "未运行" or "未设置", "on_surface_variant" end
    if view.effective_paused then return view.paused and "已暂停" or "自动暂停", "on_surface_variant" end
    return "运行中", "tertiary"
end

local function busy(output)
    if not data.connected then return true end
    local names = output == "*" and outputNames() or { output }
    if #names == 0 then return true end
    for _, name in ipairs(names) do
        local operation = data.operations[name]
        local view = data.snapshot.outputs[name]
        if not view or view.pending or (operation and operation.busy) then return true end
    end
    return false
end

local function label(text, props)
    local options = props or {}
    options.text = text
    return ui.label(options)
end

local function button(text, callback, props)
    local options = props or {}
    options.text, options.onClick = text, callback
    options.variant = options.variant or "default"
    options.controlSize = options.controlSize or "md"
    return ui.button(options)
end

local function icon(name, tooltip, callback, props)
    local options = props or {}
    options.glyph, options.tooltip = name, tooltip
    options.variant = options.variant or "ghost"
    options.width = options.width or 32
    options.controlSize = options.controlSize or "sm"
    return button("", callback, options)
end

local function caption(text)
    return label(text, { fontSize = 12, color = "on_surface_variant", maxLines = 3 })
end

local function preview(asset, width, height, imageFit)
    if asset and type(asset.thumbnail) == "string" and asset.thumbnail ~= "" then
        return ui.image({ key = "preview:" .. asset.id, path = asset.thumbnail,
            width = width, height = height, radius = 10, fit = imageFit or "cover" })
    end
    local rows = { ui.glyph({ name = asset and dynamic(asset) and "player-play" or "photo",
        size = height > 60 and 28 or 20, color = "on_surface_variant" }) }
    return ui.column({ key = "preview:" .. (asset and asset.id or "empty"), width = width, height = height, fill = "surface_variant", radius = 10,
        align = "center", justify = "center", gap = 8 }, rows)
end

local function statusChip(view)
    local text, color = status(view)
    return ui.row({ key = "status", gap = 6, align = "center" }, {
        ui.box({ width = 7, height = 7, radius = 4, fill = color }),
        label(text, { fontSize = 12, color = color == "error" and "error" or "on_surface_variant" }),
    })
end

local function propertyKey(asset)
    return asset.id .. "|" .. tostring(target or screen)
end

local function propertyPatch(asset)
    local key = propertyKey(asset)
    local draft = propertyDrafts[key]
    if not draft then return {} end
    local reset = {}
    for name in pairs(draft.reset) do table.insert(reset, name) end
    return { properties = draft.values, reset = reset, draftKey = key, draftVersion = draft.version }
end

local function queryProperties(asset)
    if asset.kind == "we_scene" then request("properties", nil, { asset = asset.id, context = propertyKey(asset), properties = propertyPatch(asset).properties, reset = propertyPatch(asset).reset }) end
end

local function changeProperty(asset, name, value, previewDraft)
    local key = propertyKey(asset)
    local previous = propertyDrafts[key]
    local draft = previous or { values = {}, reset = {}, version = 0 }
    draft.values[name], draft.reset[name] = value, nil
    draft.version += 1
    propertyDrafts[key] = draft
    if previewDraft then queryProperties(asset)
    -- Native inputs already paint their value; only the first edit needs to
    -- enable the save/discard actions. Resolve conditions when the drag ends.
    elseif not previous then scheduleRender() end
end

local function apply(asset, output)
    local extra = propertyPatch(asset)
    extra.asset, extra.fit, extra.transition = asset.id, fit, transition
    request("apply", output, extra)
end

local function retry(output)
    local operation = data.operations[output]
    if operation and operation.request then
        request(operation.action, output, operation.request)
    end
end

local function chooseScreen(name)
    screen, target = name, nil
    local asset = selected and findAsset(selected)
    if asset then queryProperties(asset) end
    render()
end

local function closeDetails()
    selected, titleEdit = nil, nil
    render()
end

local function selectAsset(asset)
    if selected ~= asset.id then
        detailTab, titleEdit = 0, nil
    end
    selected, focusSearch = asset.id, false
    if asset.kind == "we_scene" then queryProperties(asset) end
    render()
end

local function library()
    return data.library or { sources = {}, favorites = {}, playlists = {}, hidden = {} }
end

local function indexLibrary()
    local revision = data.libraryRevision or library()
    if indexedLibrary == revision then return end
    indexedLibrary = revision
    local titles = library().titles or {}
    for id, title in pairs(titles) do
        if title ~= indexedTitles[id] and assetIndex[id] then indexName(assetIndex[id]) end
    end
    for id in pairs(indexedTitles) do
        if not titles[id] and assetIndex[id] then indexName(assetIndex[id]) end
    end
    indexedTitles = titles
    hiddenIds, favoriteIds, memberPositions = {}, {}, {}
    for _, id in ipairs(library().hidden) do hiddenIds[id] = true end
    for _, id in ipairs(library().favorites) do favoriteIds[id] = true end
    for id, playlist in pairs(library().playlists) do
        local positions = {}
        for position, asset in ipairs(playlist.members) do positions[asset] = position end
        memberPositions[id] = positions
    end
    collectionCache, filterCache = nil, nil
end

local function editLibrary(edit)
    request("library-edit", nil, { edit = edit })
end

local function libraryBusy()
    return not data.connected or data.libraryOperation and data.libraryOperation.busy
end

local function navigate(id, category)
    page, collection, selected, addingTo = "library", id, nil, nil
    titleEdit = nil
    query, kindFilter, sourceFilter, gridPage = "", category or 0, 0, 1
    searchRevision += 1
    focusSearch = false
    panel.setNeedsFrameTick(false)
    render()
end

local function playlistIds()
    local ids = {}
    for id in pairs(library().playlists) do table.insert(ids, id) end
    table.sort(ids, function(a, b)
        local an, bn = library().playlists[a].name, library().playlists[b].name
        return an == bn and a < b or an < bn
    end)
    return ids
end

local function setMember(id, asset, remove)
    local playlist = library().playlists[id]
    if not playlist then return end
    local members = table.clone(playlist.members)
    local index = table.find(members, asset)
    if remove and index then table.remove(members, index)
    elseif not remove and not index then table.insert(members, asset) end
    editLibrary({ action = "playlist_members", id = id, members = members })
end

local function favorite(asset)
    editLibrary({ action = "favorite", asset_id = asset.id, favorite = not favoriteIds[asset.id] })
end

local function favoriteButton(asset, key)
    local marked = favoriteIds[asset.id] == true
    return button(marked and "🩷" or "♡", function() favorite(asset) end, {
        key = key .. ":heart", tooltip = marked and "取消收藏" or "收藏",
        fontSize = 14, width = 44, controlSize = "sm", variant = "ghost",
        selected = false, enabled = not libraryBusy(),
    })
end

local function sidebar(width)
    local rows = {
        button("壁纸", function() navigate("library") end,
            { key = "nav:library", glyph = "photo", selected = page == "library" and collection == "library" and kindFilter == 0, contentAlign = "start", variant = "ghost" }),
        ui.row({ gap = 0 }, {
            ui.spacer({ width = 24, flexGrow = 0 }),
            ui.column({ key = "categories", flexGrow = 1, gap = 2 }, {
                button("静态", function() navigate("library", 1) end,
                    { key = "kind:静态", selected = page == "library" and collection == "library" and kindFilter == 1,
                        contentAlign = "start", variant = "ghost" }),
                button("动态", function() navigate("library", 2) end,
                    { key = "kind:动态", selected = page == "library" and collection == "library" and kindFilter == 2,
                        contentAlign = "start", variant = "ghost" }),
            }),
        }),
        button("收藏", function() navigate("favorites") end,
            { key = "nav:favorites", glyph = "heart", selected = page == "library" and collection == "favorites", contentAlign = "start", variant = "ghost" }),
        ui.row({ gap = 4, align = "center" }, {
            caption("播放列表"), ui.spacer({ flexGrow = 1 }),
            icon("plus", "新建播放列表", function()
                page, selected, listForm = "library", nil, { name = "" }; render()
            end, { key = "playlist:new", enabled = not libraryBusy() }),
        }),
    }
    local playlists = {}
    for _, id in ipairs(playlistIds()) do
        local playlist = library().playlists[id]
        table.insert(playlists, button(playlist.name, function() navigate(id) end,
            { key = "nav:" .. id, glyph = "playlist", selected = page == "library" and collection == id,
                contentAlign = "start", variant = "ghost" }))
    end
    table.insert(rows, ui.scroll({ key = "playlists", flexGrow = 1, gap = 2 }, playlists))
    local names = outputNames()
    if #names > 1 then table.insert(rows, caption("显示器")) end
    for _, name in ipairs(#names > 1 and names or {}) do
        table.insert(rows, button(name, function() chooseScreen(name) end,
            { key = "output:" .. name, glyph = "device-desktop", selected = screen == name,
                contentAlign = "start", variant = "ghost" }))
    end
    table.insert(rows, button("设置", function() page = "monitor"; render() end,
        { key = "nav:settings", glyph = "settings", selected = page == "monitor", contentAlign = "start", variant = "ghost" }))
    return ui.column({ key = "sidebar", width = width, gap = 4, padding = 10,
        fill = "surface_variant/0.16", radius = 12 }, rows)
end

local function collectionAssets()
    indexAssets()
    indexLibrary()
    if collectionCache and collectionCache.id == collection then return collectionCache end
    local assets = {}
    local members = collection == "favorites" and favoriteIds or memberPositions[collection]
    for _, asset in ipairs(data.assets) do
        if not hiddenIds[asset.id] and (not members or members[asset.id]) then
            table.insert(assets, asset)
        end
    end
    collectionCache = { id = collection, assets = assets }
    filterCache = nil
    return collectionCache
end

local function filteredAssets()
    local base = collectionAssets()
    if filterCache and filterCache.query == query and filterCache.kind == kindFilter
        and filterCache.source == sourceFilter and filterCache.order == sortOrder then return filterCache.assets end
    local assets = {}
    local search = query:lower()
    local positions = memberPositions[collection]
    local order = collection == "favorites" and 0 or sortOrder
    for _, asset in ipairs(base.assets) do
        local matchesKind = kindFilter == 0 or (kindFilter == 1 and not dynamic(asset))
            or (kindFilter == 2 and dynamic(asset))
        local matchesSource = sourceFilter == 0 or (sourceFilter == 1 and not source(asset))
            or (sourceFilter == 2 and source(asset))
        if matchesKind and matchesSource and assetSearch[asset.id]:find(search, 1, true) then
            table.insert(assets, asset)
        end
    end
    table.sort(assets, function(a, b)
        if positions then
            return positions[a.id] < positions[b.id]
        end
        if order == 2 and a.kind ~= b.kind then return a.kind < b.kind end
        local an, bn = assetNames[a.id], assetNames[b.id]
        if an == bn then return a.id < b.id end
        return order == 1 and an > bn or order ~= 1 and an < bn
    end)
    filterCache = { query = query, kind = kindFilter, source = sourceFilter, order = sortOrder, assets = assets }
    return assets
end

local function resetFilters()
    query, sourceFilter, gridPage = "", 0, 1
    searchRevision += 1 -- Inputs are uncontrolled; a new key clears the native editor.
    focusSearch = false
    render()
end

local function libraryTools(width, layout)
    local selectors = {
        ui.select({ key = "source", options = { "来源", "本地", "Wallpaper Engine" },
            selectedIndex = sourceFilter, width = width >= 780 and 148 or 112, controlSize = "md",
            onChange = function(value) sourceFilter = tonumber(value) or 0; gridPage = 1; render() end }),
        ui.select({ key = "sort", options = { "名称 A → Z", "名称 Z → A", "按类型" },
            selectedIndex = sortOrder, width = width >= 780 and 140 or 124, controlSize = "md",
            onChange = function(value) sortOrder = tonumber(value) or 0; gridPage = 1; render() end }),
        icon("layout-grid", "网格视图", function() viewMode = "grid"; gridPage = 1; render() end,
            { key = "view:grid", selected = viewMode == "grid" }),
        icon("list", "列表视图", function() viewMode = "list"; gridPage = 1; render() end,
            { key = "view:list", selected = viewMode == "list" }),
    }
    if collection == "favorites" or library().playlists[collection] then table.remove(selectors, 2) end
    if layout.pages > 1 then
        local text = tostring(gridPage) .. "/" .. tostring(layout.pages)
        table.insert(selectors, selectors[2].props.key == "sort" and 3 or 2,
            ui.row({ key = "pagination", width = 116, gap = 2, align = "center" }, {
                icon("chevron-left", "上一页", function() gridPage -= 1; render() end,
                    { key = "page:prev", width = 28, enabled = gridPage > 1 }),
                -- Native labels measure their text; a fixed flex cell keeps the toolbar still.
                ui.row({ key = "page:slot", width = 56, align = "center", justify = "center" }, {
                    label(text, { key = "page:count", textAlign = "center", fontSize = 12, maxLines = 1 }),
                }),
                icon("chevron-right", "下一页", function() gridPage += 1; render() end,
                    { key = "page:next", width = 28, enabled = gridPage < layout.pages }),
            }))
    end
    return selectors
end

local function assetCard(asset, width, list)
    local view = data.snapshot.outputs[screen]
    local current = data.connected and view and view.current == asset.id
    local loading = data.connected and view and view.pending == asset.id
    local metadata = { label(typeText(asset), { fontSize = 12, color = "on_surface_variant", maxLines = 1, flexGrow = 1 }) }
    if loading or current or asset.error then
        table.insert(metadata, label(loading and "加载中…" or current and "使用中" or "不可用",
            { fontSize = 11, color = loading and "primary" or current and "on_surface_variant" or "error" }))
    end
    local cardActions = {
        favoriteButton(asset, "favorite:" .. asset.id),
    }
    if addingTo then
        local member = memberPositions[addingTo] and memberPositions[addingTo][asset.id] ~= nil
        table.insert(cardActions, icon(member and "check" or "plus", member and "从列表移除" or "加入列表", function()
            setMember(addingTo, asset.id, member)
        end, { key = "member:" .. asset.id, selected = member, enabled = not libraryBusy(), width = 28 }))
    end
    local textRows = {
        ui.row({ gap = 4, align = "center" }, {
            label(displayName(asset), { flexGrow = 1, maxLines = 1, fontSize = 14, fontWeight = "medium",
                color = "on_surface" }),
            ui.glyph({ name = dynamic(asset) and "player-play" or "photo", size = 12, color = "on_surface_variant" }),
            ui.row({ gap = 0 }, cardActions),
        }),
    }
    if list then table.insert(textRows, ui.row({ gap = 6, align = "center" }, metadata))
    elseif loading or current or asset.error then
        table.insert(textRows, metadata[#metadata])
    end
    local props = { key = asset.id, width = width, gap = list and 12 or 8, padding = 8, radius = 12,
        fill = "surface_variant/0.16",
        border = selected == asset.id and "primary" or "outline/0.2",
        borderWidth = selected == asset.id and 2 or 1,
        onClick = function() selectAsset(asset) end,
    }
    local playlist = library().playlists[collection]
    if playlist and not addingTo then
        local id, index = collection, memberPositions[collection][asset.id]
        local function move(offset)
            local members = table.clone(playlist.members)
            members[index], members[index + offset] = members[index + offset], members[index]
            editLibrary({ action = "playlist_members", id = id, members = members })
        end
        table.insert(textRows, ui.row({ gap = 4 }, {
            caption(tostring(index)), ui.spacer({ flexGrow = 1 }),
            icon("arrow-up", "前移", function() move(-1) end, { key = "move-up:" .. asset.id, width = 26, enabled = index > 1 and not libraryBusy() }),
            icon("arrow-down", "后移", function() move(1) end, { key = "move-down:" .. asset.id, width = 26, enabled = index < #playlist.members and not libraryBusy() }),
            icon("minus", "移出列表", function() setMember(id, asset.id, true) end, { key = "remove-member:" .. asset.id, width = 26, enabled = not libraryBusy() }),
        }))
    end
    if list then
        props.align = "center"
        return ui.row(props, { preview(asset, 112, 63), ui.column({ gap = 6, flexGrow = 1 }, textRows) })
    end
    local rows = { preview(asset, width - 16, math.floor((width - 16) * 2 / 3)),
        ui.column({ gap = 4, paddingH = 4, paddingV = 0 }, textRows) }
    return ui.column(props, rows)
end

local function collectionControls(sourceId)
    local rotation = data.snapshot.rotations and data.snapshot.rotations[screen]
    local saved = rotation and rotation.settings
    local key = tostring(screen) .. "|" .. sourceId
    local draft = rotationDrafts[key]
    if not draft then
        draft = saved and saved.source == sourceId and table.clone(saved)
            or { source = sourceId, enabled = false, mode = "ordered", interval_seconds = 300, fit = "cover", transition = "fade" }
        rotationDrafts[key] = draft
    end
    local running = saved and saved.enabled and saved.source == sourceId
    local intervals, intervalLabels = { 30, 60, 300, 900, 1800, 3600 }, { "30 秒", "1 分钟", "5 分钟", "15 分钟", "30 分钟", "1 小时" }
    local intervalIndex = table.find(intervals, draft.interval_seconds)
    if not intervalIndex then table.insert(intervals, draft.interval_seconds); table.insert(intervalLabels, tostring(draft.interval_seconds) .. " 秒"); intervalIndex = #intervals end
    local function configure(enabled)
        local settings = table.clone(draft)
        settings.enabled, settings.changed = enabled, nil
        request("rotation", screen, { settings = settings })
    end
    local options = {
        ui.select({ key = "rotation-mode:" .. key, options = { "顺序", "随机" }, width = 84, controlSize = "md",
            selectedIndex = draft.mode == "random" and 1 or 0, onChange = function(value)
                draft.mode, draft.changed = value == "1" and "random" or "ordered", true; render()
            end }),
        ui.select({ key = "rotation-interval:" .. key, options = intervalLabels, width = 100, controlSize = "md",
            selectedIndex = intervalIndex - 1, onChange = function(value)
                draft.interval_seconds, draft.changed = intervals[(tonumber(value) or 0) + 1] or 300, true; render()
            end }),
        icon(running and not draft.changed and "player-pause" or running and "refresh" or "player-play",
            running and not draft.changed and "暂停轮播" or running and "更新轮播" or "开启轮播", function()
            configure(not (running and not draft.changed))
        end, { key = "rotation:toggle", width = 38, controlSize = "md",
            enabled = not busy(screen), variant = "default" }),
        icon("player-skip-forward", "下一张", function() request("next", screen) end,
            { key = "rotation:next", enabled = running and not busy(screen) }),
    }
    local playlist = library().playlists[sourceId]
    local actions = {
        playlist and icon("plus", "添加壁纸", function()
            navigate("library"); addingTo = sourceId; render()
        end, { key = "playlist:add", width = 38, controlSize = "md", variant = "default", enabled = not libraryBusy() })
            or caption("轮播"),
        ui.spacer({ flexGrow = 1 }),
    }
    for _, node in ipairs(options) do table.insert(actions, node) end
    if playlist then
        table.insert(actions, icon("pencil", "重命名", function() listForm = { id = sourceId, name = playlist.name }; render() end,
            { key = "playlist:rename", enabled = not libraryBusy() }))
        table.insert(actions, icon("trash", "删除播放列表", function() deletingList = sourceId; render() end,
            { key = "playlist:delete", enabled = not libraryBusy() }))
    end
    local rows = { ui.row({ gap = 6, align = "center" }, actions) }
    if running and rotation.error then table.insert(rows, label(rotation.error.message, { fontSize = 12, color = "error", maxLines = 2 })) end
    return ui.column({ key = "rotation-controls", gap = 6, paddingV = 4 }, rows)
end

local function playlistTools()
    local rows = {}
    if listForm then
        local form = listForm
        table.insert(rows, ui.row({ gap = 6 }, {
            ui.input({ key = "playlist-name:" .. tostring(form.id), value = form.name, placeholder = "播放列表名称", flexGrow = 1, focus = true, enabled = not libraryBusy(),
                controlSize = "md", onChange = function(value) form.name = value end }),
            button("保存", function()
                if form.name:match("%S") then
                    editLibrary(form.id and { action = "rename_playlist", id = form.id, name = form.name }
                        or { action = "create_playlist", name = form.name })
                    render()
                end
            end, { key = "playlist:save", enabled = not libraryBusy(), variant = "primary" }),
            icon("x", "取消", function() listForm = nil; render() end, { key = "playlist:cancel" }),
        }))
    end
    if addingTo then
        local playlist = library().playlists[addingTo]
        table.insert(rows, ui.row({ gap = 6, padding = 8, fill = "surface_variant/0.16", radius = 8, align = "center" }, {
            label("添加到「" .. (playlist and playlist.name or "列表") .. "」", { flexGrow = 1, maxLines = 1 }),
            button("完成", function() navigate(addingTo) end, { key = "playlist:done", variant = "primary" }),
        }))
    elseif collection ~= "library" then
        table.insert(rows, collectionControls(collection))
        local playlist = library().playlists[collection]
        if playlist then
            local id = collection
            if deletingList == id then
                table.insert(rows, ui.row({ gap = 6, align = "center" }, {
                    caption("删除此列表？"), ui.spacer({ flexGrow = 1 }),
                    button("删除", function() editLibrary({ action = "delete_playlist", id = id }); deletingList = nil end,
                        { key = "playlist:confirm-delete", variant = "destructive", enabled = not libraryBusy() }),
                    button("取消", function() deletingList = nil; render() end, { variant = "ghost" }),
                }))
            end
        end
    end
    return #rows > 0 and ui.column({ key = "collection-tools", gap = 8 }, rows) or nil
end

local function galleryLayout(width)
    width -= 14 -- Reserve a gutter so the native scrollbar never covers a card.
    local assets = filteredAssets()
    local columns = viewMode == "list" and 1 or math.clamp(math.floor((width + 10) / 192), 1, 5)
    local cardWidth = math.floor((width - (columns - 1) * 10) / columns)
    local pageSize = viewMode == "list" and 24 or columns * 5
    local pages = math.max(1, math.ceil(#assets / pageSize))
    gridPage = math.clamp(gridPage, 1, pages)
    return { width = width, assets = assets, columns = columns, cardWidth = cardWidth, pageSize = pageSize, pages = pages }
end

local function gallery(layout)
    local assets, columns, cardWidth, pageSize = layout.assets, layout.columns, layout.cardWidth, layout.pageSize
    local grid, row = {}, {}
    local last = math.min(gridPage * pageSize, #assets)
    for index = (gridPage - 1) * pageSize + 1, last do
        table.insert(row, assetCard(assets[index], cardWidth, viewMode == "list"))
        if #row == columns or index == last then
            table.insert(grid, ui.row({ key = "row:" .. tostring(index), gap = 10, align = "start" }, row))
            row = {}
        end
    end
    local playlist = library().playlists[collection]
    if playlist and query == "" and kindFilter == 0 and sourceFilter == 0 then
        for _, id in ipairs(playlist.members) do
            if not findAsset(id) or hiddenIds[id] then
                local sourceId = collection
                table.insert(grid, ui.row({ key = "missing:" .. id, gap = 8, align = "center", padding = 8 }, {
                    label(assetName(id), { flexGrow = 1, fontSize = 12, maxLines = 1 }),
                    caption(hiddenIds[id] and "已隐藏" or "文件不可用"),
                    icon("minus", "移出列表", function() setMember(sourceId, id, true) end,
                        { key = "remove-missing:" .. id, enabled = not libraryBusy() }),
                }))
            end
        end
    end
    if #grid == 0 then
        local filtered = query ~= "" or sourceFilter ~= 0
        local emptyCollection = collection ~= "library" and not filtered
        local title = data.scanning and "加载壁纸…" or filtered and "没有匹配的壁纸"
            or collection == "favorites" and "暂无收藏" or playlist and "播放列表为空"
            or kindFilter == 1 and "暂无静态壁纸" or kindFilter == 2 and "暂无动态壁纸" or "暂无壁纸"
        table.insert(grid, ui.column({ padding = 24, gap = 12, align = "center" }, {
            ui.glyph({ name = collection == "favorites" and "heart" or "photo", size = 32, color = "on_surface_variant" }),
            label(title, { fontSize = 16, fontWeight = "medium" }),
            button(filtered and "清除筛选" or emptyCollection and not playlist and "浏览壁纸" or "添加壁纸", function()
                if filtered then resetFilters()
                elseif emptyCollection then
                    local id = playlist and collection or nil
                    navigate("library"); addingTo = id; render()
                else page, settingsTab = "monitor", 1; render() end
            end, { key = "empty-action", variant = "default" }),
        }))
    end
    local children = { ui.scroll({ key = "grid:" .. collection .. ":" .. viewMode .. ":" .. tostring(gridPage), flexGrow = 1, gap = 10 }, grid) }
    local tools = playlistTools()
    if tools then table.insert(children, 1, tools) end
    return ui.column({ key = "gallery", flexGrow = 1, gap = 12 }, children)
end

local function fitControls()
    return ui.column({ gap = 12 }, {
        ui.column({ gap = 4 }, {
            caption("适配"),
            ui.select({ key = "fit", options = { "填满屏幕 · 裁剪", "完整显示 · 留边", "拉伸至屏幕" },
                selectedIndex = (table.find(fits, fit) or 1) - 1, controlSize = "md",
                onChange = function(value) fit = fits[(tonumber(value) or 0) + 1] or "cover"; render() end }),
        }),
        ui.column({ gap = 4 }, {
            caption("切换"),
            ui.select({ key = "transition", options = { "淡入淡出", "圆形扩散", "蜂巢波纹", "螺旋绽放", "斜纹交织" },
                selectedIndex = (table.find(transitions, transition) or 1) - 1, controlSize = "md",
                onChange = function(value) transition = transitions[(tonumber(value) or 0) + 1] or "fade"; render() end }),
        }),
    })
end

local function failure(output, message, operation)
    local rows = {
        label(output .. " · 操作失败", { fontWeight = "medium", color = "error" }),
        label(message, { fontSize = 12, color = "on_surface_variant", maxLines = 3 }),
    }
    if operation and operation.action == "apply" then table.insert(rows, caption("当前桌面保留原壁纸。")) end
    if operation and operation.request then
        table.insert(rows, button("重试此屏", function() retry(output) end,
            { key = "retry:" .. output, glyph = "refresh", enabled = not busy(output), variant = "default" }))
    end
    return ui.column({ gap = 8, padding = 12, fill = "error/0.08", radius = 10 }, rows)
end

local propertyTypes = { bool = true, slider = true, combo = true, color = true, textinput = true }
local propertyLocale = "zh_CN"
for _, variable in ipairs({ "LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG" }) do
    local value = noctalia.getenv(variable)
    if value and value ~= "" then propertyLocale = value:lower(); break end
end
local chineseProperties = propertyLocale:sub(1, 2) == "zh"

local function isChineseText(text)
    local cjk, latin = 0, 0
    for _, code in utf8.codes(text) do
        if code >= 0x3400 and code <= 0x9FFF or code >= 0x3040 and code <= 0x30FF then cjk += 1
        elseif code >= 65 and code <= 90 or code >= 97 and code <= 122 then latin += 1 end
    end
    -- A Han character in an emoticon (e.g. (*´∀人)) does not make English prose Chinese.
    return cjk > 0 and cjk * 2 >= latin
end

local function plainText(text)
    return (tostring(text):gsub("<[bB][rR]%s*/?%s*>", "\n")
        :gsub("</[pP]>", "\n"):gsub("</[hH]%d>", "\n"):gsub("<[^>]*>", "")
        :gsub("&nbsp;", " "):gsub("&amp;", "&"):gsub("&quot;", '"')
        :gsub("&#39;", "'"):gsub("&lt;", "<"):gsub("&gt;", ">")
        :gsub("^%s+", ""):gsub("%s+$", ""))
end

local function localizedText(text)
    text = plainText(text)
    local first, second = text:match("^(.-)%s*/%s*(.+)$")
    if first and not text:find("://", 1, true) and isChineseText(first) ~= isChineseText(second) then
        text = isChineseText(first) == chineseProperties and first or second
    end
    -- Author notes often put translations in separate HTML paragraphs.
    local native, foreign = {}, {}
    for line in text:gmatch("[^\n]+") do
        table.insert(isChineseText(line) and native or foreign, line)
    end
    if #native > 0 and #foreign > 0 then text = table.concat(chineseProperties and native or foreign, "\n") end
    return (text:gsub("^%s+", ""):gsub("%s+$", ""))
end

local function propertyText(definition)
    if definition.text == "ui_browse_properties_scheme_color" then
        return chineseProperties and "主题色" or "Theme color"
    end
    return localizedText(definition.text or definition.key)
end

local function propertySections(definitions)
    local fields, notes = {}, {}
    local group, fieldGroup, noteGroup = nil, nil, nil
    for _, definition in ipairs(definitions or {}) do
        if definition.type == "group" then group = definition
        else
            local controls = propertyTypes[definition.type] == true
            local destination = controls and fields or notes
            if group and (controls and fieldGroup ~= group or not controls and noteGroup ~= group) then
                table.insert(destination, group)
                if controls then fieldGroup = group else noteGroup = group end
            end
            table.insert(destination, definition)
        end
    end
    return fields, notes
end

local function authorInfo(asset)
    local entry = data.properties and data.properties[asset.id]
    if not entry or not entry.reply then return nil end
    local _, notes = propertySections(entry.reply.definitions)
    local rows = {}
    for _, definition in ipairs(notes) do
        local text = propertyText(definition)
        if text ~= "" then
            table.insert(rows, label(text, { fontSize = 12, maxLines = 40,
                fontWeight = definition.type == "group" and "bold" or "normal",
                color = definition.type == "group" and "on_surface" or "on_surface_variant" }))
        end
        -- Preserve authored image/link destinations without loading remote content.
        for tag in tostring(definition.text or ""):gmatch("<[^>]+>") do
            local attr = tag:lower():match("^<img%s") and "src" or tag:lower():match("^<a%s") and "href"
            if attr then
                local url = tag:match(attr .. '%s*=%s*"([^"]+)"') or tag:match(attr .. "%s*=%s*'([^']+)'") or tag:match(attr .. "%s*=%s*([^%s>]+)")
                if url and (url:match("^https://") or url:match("^http://")) then
                    local destination = url:gsub("&amp;", "&")
                    table.insert(rows, button(attr == "src" and "复制附图链接" or "复制作者链接", function()
                        noctalia.copyToClipboard(destination, "text/plain")
                    end, { glyph = "copy", variant = "ghost", contentAlign = "start" }))
                end
            end
        end
    end
    return #rows > 0 and ui.column({ key = "author-info", gap = 8 }, rows) or nil
end

local function colorHex(value)
    if type(value) ~= "table" then return "#FFFFFF" end
    return string.format("#%02X%02X%02X", math.round((value[1] or 1) * 255),
        math.round((value[2] or 1) * 255), math.round((value[3] or 1) * 255))
end

local function propertyEditor(asset)
    if asset.kind ~= "we_scene" then return caption("没有可编辑属性") end
    local entry = data.properties and data.properties[asset.id]
    if not entry or not entry.reply then
        return ui.column({ gap = 8 }, {
            caption(entry and entry.error or "正在读取属性…"),
            entry and entry.error and button("重试", function() queryProperties(asset) end, { variant = "default" }) or nil,
        })
    end
    local reply = entry.reply
    local output = reply.outputs[target == "*" and screen or target or screen]
    local values, visible = output and output.values or reply.values, output and output.visible or reply.visible
    local draft = propertyDrafts[propertyKey(asset)]
    local rows, count = {}, 0
    local fields = propertySections(reply.definitions)
    for _, definition in ipairs(fields) do
        local name, key = propertyText(definition), definition.key
        if not visible or visible[key] ~= false then
            local id = "property:" .. propertyKey(asset) .. ":" .. key
            local value = values[key]
            if draft then
                if draft.reset[key] then value = definition.default
                elseif draft.values[key] ~= nil then value = draft.values[key] end
            end
            local enabled = data.connected and definition.editable and not asset.error
            local control
            if definition.type == "group" then
                table.insert(rows, ui.column({ paddingV = 4 }, {
                    label(name, { fontSize = 14, fontWeight = "bold", maxLines = 2 }),
                }))
            else
                count += 1
                if definition.type == "bool" then
                    control = ui.toggle({ key = id, checked = value == true, enabled = enabled,
                        onChange = function(chosen) changeProperty(asset, key, chosen == "true", true) end })
                elseif definition.type == "slider" and enabled then
                    control = ui.column({ gap = 4, flexGrow = 1 }, {
                        ui.slider({ key = id, min = definition.min, max = definition.max,
                            step = definition.step or (definition.fraction and 0.01 or 1), value = value,
                            onChange = function(chosen) changeProperty(asset, key, tonumber(chosen), false) end,
                            onDragEnd = function() queryProperties(asset) end }),
                    })
                elseif definition.type == "combo" and enabled then
                    local options, index = {}, 0
                    for optionIndex, option in ipairs(definition.options or {}) do
                        table.insert(options, localizedText(option.label or option.value))
                        if value == option.value then index = optionIndex - 1 end
                    end
                    control = ui.select({ key = id, options = options, selectedIndex = index, controlSize = "md",
                        onChange = function(chosen)
                            local option = definition.options[(tonumber(chosen) or 0) + 1]
                            if option then changeProperty(asset, key, option.value, true) end
                        end })
                elseif definition.type == "color" and enabled then
                    local hex = colorHex(value)
                    local picker = button(hex, function()
                        local destination = propertyKey(asset)
                        noctalia.openColorPicker(hex, function(chosen)
                            if chosen and destination == propertyKey(asset) then
                                changeProperty(asset, key, { tonumber(chosen:sub(2, 3), 16) / 255,
                                    tonumber(chosen:sub(4, 5), 16) / 255, tonumber(chosen:sub(6, 7), 16) / 255 }, true)
                            end
                        end)
                    end, { key = id, glyph = "palette", variant = "default", flexGrow = 1 })
                    control = ui.row({ gap = 8, align = "center" }, {
                        ui.box({ width = 22, height = 22, fill = hex, radius = 4 }), picker,
                    })
                elseif definition.type == "textinput" and enabled then
                    control = ui.input({ key = id .. ":" .. tostring(propertyRevisions[propertyKey(asset)] or 0) .. ":" .. tostring(entry.revision or 0),
                        value = tostring(value or ""), controlSize = "md",
                        onChange = function(chosen) changeProperty(asset, key, chosen, false) end,
                        onSubmit = function() queryProperties(asset) end })
                else control = caption(definition.error or "只读属性") end
                local title = { label(name, { fontSize = 13, flexGrow = 1, maxLines = 2 }) }
                if definition.type == "slider" then table.insert(title, caption(tostring(value))) end
                if definition.type == "bool" then table.insert(title, control) end
                local children = { ui.row({ gap = 8, align = "center" }, title) }
                if definition.type ~= "bool" then table.insert(children, control) end
                local warning = output and output.warnings and output.warnings[key]
                if warning then table.insert(children, label(warning, { fontSize = 11, color = "error", maxLines = 2 })) end
                table.insert(rows, ui.column({ key = "field:" .. id, gap = 4 }, children))
            end
        end
    end
    if count == 0 then table.insert(rows, caption("没有可编辑属性")) end
    if count > 0 then
        table.insert(rows, button("恢复默认", function()
            local key = propertyKey(asset)
            local previous = propertyDrafts[key]
            local reset = {}
            for _, definition in ipairs(reply.definitions) do if definition.editable then reset[definition.key] = true end end
            for name in pairs(output and output.overrides or {}) do reset[name] = true end
            propertyDrafts[key] = { values = {}, reset = reset, version = previous and previous.version + 1 or 1 }
            propertyRevisions[key] = (propertyRevisions[key] or 0) + 1
            queryProperties(asset)
            render()
        end, { key = "properties:reset", glyph = "restore", variant = "ghost", enabled = data.connected }))
    end
    if entry.error then table.insert(rows, label(entry.error, { fontSize = 12, color = "error", maxLines = 2 })) end
    return ui.column({ key = "property-editor", gap = 10 }, rows)
end

local function details(asset, width, inWorkspace)
    local names = outputNames()
    local options = table.clone(names)
    table.insert(options, "全部显示器")
    local destination = target or screen
    local index = destination == "*" and #names or (table.find(names, destination) or 1) - 1
    local title = {}
    if titleEdit and titleEdit.asset == asset.id then
        local draft = titleEdit
        local function save(value)
            if not libraryBusy() then
                editLibrary({ action = "rename_asset", asset_id = asset.id, title = value or draft.text })
            end
        end
        table.insert(title, ui.input({ key = "title-input:" .. asset.id, value = draft.text,
            placeholder = originalName(asset),
            flexGrow = 1, controlSize = "md", focus = true, enabled = not libraryBusy(),
            onChange = function(value) draft.text = value end, onSubmit = save }))
        table.insert(title, icon("check", "保存标题 · 留空恢复原标题", function() save() end,
            { key = "title:save", enabled = not libraryBusy() }))
        table.insert(title, icon("arrow-back-up", "取消修改", function() titleEdit = nil; render() end,
            { key = "title:cancel", enabled = not libraryBusy() }))
    else
        table.insert(title, label(displayName(asset), { flexGrow = 1, fontSize = 16, fontWeight = "bold", maxLines = 2 }))
        table.insert(title, icon("pencil", "修改标题", function()
            titleEdit = { asset = asset.id, text = displayName(asset) }; render()
        end, { key = "title:edit", enabled = not libraryBusy() }))
    end
    table.insert(title, icon("x", "关闭详情", closeDetails, { key = "close-details" }))
    local rows = { ui.row({ key = "detail-title", gap = 6, align = "center" }, title) }
    local ids, playlistOptions = playlistIds(), { "加入播放列表…" }
    for _, id in ipairs(ids) do table.insert(playlistOptions, library().playlists[id].name) end
    table.insert(rows, ui.row({ gap = 6 }, {
        favoriteButton(asset, "detail:favorite"),
        ui.select({ key = "add-playlist:" .. tostring(listRevision), options = playlistOptions, selectedIndex = 0,
            flexGrow = 1, controlSize = "md", enabled = #ids > 0 and not libraryBusy(), onChange = function(value)
                local id = ids[tonumber(value) or 0]
                if id then setMember(id, asset.id, false); listRevision += 1; render() end
            end }),
    }))
    local tabs = {}
    local hasProperties = asset.kind == "we_scene"
    for tabIndex, name in ipairs(hasProperties and { "属性", "画面", "信息" } or { "画面", "信息" }) do
        local tab = hasProperties and tabIndex - 1 or tabIndex
        table.insert(tabs, button(name, function() detailTab = tab; render() end,
            { key = "detail-tab:" .. tostring(tab), selected = detailTab == tab or not hasProperties and detailTab == 0 and tab == 1,
                flexGrow = 1, variant = "ghost" }))
    end
    table.insert(rows, ui.row({ gap = 4 }, tabs))
    if detailTab == 0 and hasProperties then
        table.insert(rows, propertyEditor(asset))
    elseif detailTab <= 1 then
        table.insert(rows, fitControls())
    else
        table.insert(rows, caption(typeText(asset)))
        if library().titles and library().titles[asset.id] then
            table.insert(rows, ui.column({ gap = 4 }, {
                caption("原标题"), label(originalName(asset), { fontSize = 13, maxLines = 3 }),
            }))
        end
        table.insert(rows, ui.column({ gap = 4 }, {
            caption("位置"),
            ui.row({ gap = 8, align = "start" }, {
                label(asset.path, { fontSize = 12, maxLines = 4, flexGrow = 1 }),
                icon("copy", "复制路径", function() noctalia.copyToClipboard(asset.path, "text/plain") end,
                    { key = "asset:copy-path" }),
            }),
        }))
        if asset.project and asset.project.file then
            table.insert(rows, caption("项目入口 · " .. asset.project.file))
        end
        table.insert(rows, button("隐藏壁纸", function()
            editLibrary({ action = "hide", asset_id = asset.id, hidden = true }); closeDetails()
        end, { key = "asset:hide", glyph = "eye-off", variant = "ghost", contentAlign = "start",
            enabled = not libraryBusy(), tooltip = "从图库和轮播隐藏，原文件保留" }))
        local info = authorInfo(asset)
        if info then
            table.insert(rows, ui.separator({}))
            table.insert(rows, label("作者说明", { fontSize = 14, fontWeight = "bold" }))
            table.insert(rows, info)
        end
    end
    if asset.error then
        table.insert(rows, label(asset.error.message, { fontSize = 12, color = "error", maxLines = 4 }))
    end
    local destinations = destination == "*" and names or { destination }
    local applied = #destinations > 0
    local applying = false
    for _, name in ipairs(destinations) do
        local view, operation = data.snapshot.outputs[name], data.operations[name]
        applied = applied and view ~= nil and view.current == asset.id and data.connected
        if operation and operation.asset == asset.id and operation.busy or view and view.pending == asset.id then
            applying = true
            table.insert(rows, label(name .. " · 加载中…", { fontSize = 12, color = "primary" }))
        elseif operation and operation.asset == asset.id and operation.error then
            table.insert(rows, failure(name, operation.error, operation))
        end
    end
    local draft = propertyDrafts[propertyKey(asset)]
    local canSave = applied and draft ~= nil and detailTab ~= 1
    for _, name in ipairs(destinations) do
        local view = data.snapshot.outputs[name]
        canSave = canSave and view and view.properties_available
    end
    local titleRow = table.remove(rows, 1)
    local actions = { ui.separator({}) }
    if #names > 1 then
        table.insert(actions, ui.row({ gap = 8, align = "center" }, {
            caption("应用到"),
            ui.select({ key = "target", options = options, selectedIndex = index,
                flexGrow = 1, controlSize = "md", onChange = function(value)
                    local chosen = (tonumber(value) or 0) + 1
                    target = chosen > #names and "*" or names[chosen]
                    queryProperties(asset)
                    render()
                end }),
        }))
    end
    local submit = {}
    if draft then
        table.insert(submit, button("撤销", function()
            propertyDrafts[propertyKey(asset)] = nil
            propertyRevisions[propertyKey(asset)] = (propertyRevisions[propertyKey(asset)] or 0) + 1
            queryProperties(asset); render()
        end, { key = "properties:discard", glyph = "arrow-back-up", variant = "ghost", tooltip = "撤销属性修改" }))
    end
    table.insert(submit, button(applying and "应用中…" or canSave and "保存属性" or "应用", function()
        if canSave then
            local extra = propertyPatch(asset); extra.asset = asset.id
            request("set-properties", destination, extra)
        else apply(asset, destination) end
    end, { key = "apply", glyph = "check", variant = "primary", flexGrow = 1, enabled = not asset.error and not busy(destination) }))
    table.insert(actions, ui.row({ gap = 8 }, submit))
    -- Only stacked details grow vertically; the right column keeps its fixed width.
    return ui.column({ key = "details:" .. asset.id, width = width, flexGrow = inWorkspace and 1 or 0, gap = 8, padding = 12,
        fill = "surface_variant/0.16", radius = 12 }, {
        titleRow,
        ui.scroll({ key = "detail-content:" .. asset.id, flexGrow = 1, gap = 12 }, rows),
        -- The primary action never scrolls away with the preview or error messages.
        ui.column({ key = "apply-actions", gap = 8 }, actions),
    })
end

local function monitorSettings(width)
    local view = data.snapshot.outputs[screen]
    if not view then return ui.column({ flexGrow = 1 }, { caption("没有可用显示器") }) end
    local enabled, output = not busy(screen), screen
    local function slider(key, minimum, maximum)
        local id = output .. ":" .. key
        return ui.slider({ key = id, min = minimum, max = maximum, step = 1,
            value = sliderValues[id] or view[key], enabled = enabled,
            onChange = function(value)
                sliderValues[id] = tonumber(value)
                scheduleRender()
            end,
            -- Noctalia's onDragEnd has no value argument; keep drafts through snapshots.
            onDragEnd = function()
                local value = sliderValues[id]
                sliderValues[id] = nil
                if value ~= nil then request("playback", output, { patch = { [key] = value } }) end
            end })
    end
    local playback = {
        label("播放控制", { fontWeight = "bold", fontSize = 16 }),
        ui.row({ gap = 12, align = "center" }, {
            label("暂停播放", { flexGrow = 1 }),
            ui.toggle({ key = "pause", checked = view.paused, enabled = enabled,
                onChange = function(value) request(value == "true" and "pause" or "resume", output) end }),
        }),
    }
    if view.effective_paused then table.insert(playback, caption("暂停原因 · " .. reasonText(view.pause_reasons))) end
    table.insert(playback, ui.row({ gap = 12, align = "center" }, {
        label("静音", { flexGrow = 1 }),
        ui.toggle({ key = "mute", checked = view.mute, enabled = enabled,
            onChange = function(value) request("playback", output, { patch = { mute = value == "true" } }) end }),
    }))
    if view.effective_mute and not view.mute then table.insert(playback, caption("自动静音 · " .. reasonText(view.mute_reasons))) end
    for _, setting in ipairs({ { "volume", "音量", 0, 100, "%" }, { "fps", "帧率上限", 1, 240, " FPS" } }) do
        table.insert(playback, ui.column({ gap = 4 }, {
            ui.row({ align = "center" }, {
                label(setting[2], { flexGrow = 1 }),
                caption(tostring(sliderValues[output .. ":" .. setting[1]] or view[setting[1]]) .. setting[5]),
            }),
            slider(setting[1], setting[3], setting[4]),
        }))
    end
    local rows = {}
    local background = data.backgrounds and data.backgrounds[output]
    if background and background.error then
        table.insert(rows, label("背景同步失败 · " .. background.error, { fontSize = 12, color = "error", maxLines = 3 }))
    end
    local sections = {}
    if view.current and view.kind ~= "image" then
        table.insert(sections, ui.column({ key = "monitor-playback", padding = 16, gap = 12, flexGrow = width >= 740 and 1 or 0,
            radius = 12, fill = "surface_variant/0.16" }, playback))
    end
    table.insert(sections, ui.column({ key = "monitor-appearance", padding = 16, gap = 12, flexGrow = width >= 740 and 1 or 0,
        radius = 12, fill = "surface_variant/0.16" }, {
        label("画面", { fontWeight = "bold", fontSize = 16 }), fitControls(),
        ui.row({ gap = 8 }, {
            button("应用", function() apply({ id = view.current }, output) end,
                { key = "settings:apply", glyph = "check", flexGrow = 1, enabled = enabled and view.current ~= nil, variant = "primary" }),
            button("关闭壁纸", function() request("release", output) end,
                { key = "settings:release", glyph = "player-stop", tooltip = "关闭此显示器的壁纸", variant = "ghost",
                    enabled = enabled and (view.current ~= nil or view.pending ~= nil) }),
        }),
    }))
    table.insert(rows, width >= 740 and ui.row({ gap = 12, align = "start" }, sections) or ui.column({ gap = 12 }, sections))
    local operation = data.operations[output]
    if operation and operation.error then table.insert(rows, failure(output, operation.error, operation))
    elseif view.error then table.insert(rows, failure(output, view.error.message)) end
    return ui.scroll({ key = "monitor:" .. output, flexGrow = 1, gap = 12 }, rows)
end

local function importAsset(path)
    path = path:match("^%s*(.-)%s*$") or ""
    if path == "" then return end
    pickerError = nil
    editLibrary({ action = "import", path = path:sub(1, 1) == "~" and noctalia.expandPath(path) or path })
end

local function pickAsset(directory)
    if not noctalia.commandExists("zenity") then
        pickerError = "无法打开文件选择器，请粘贴路径添加"; render(); return
    end
    local argv = { "zenity", "--file-selection", "--title=" .. (directory and "添加壁纸目录" or "添加壁纸文件") }
    if directory then table.insert(argv, "--directory") end
    pickerError = nil
    local accepted = noctalia.runAsync(argv, function(result)
        if result.exitCode == 0 and not result.timedOut then importAsset(result.stdout:gsub("[\r\n]+$", ""))
        elseif result.timedOut then pickerError = "文件选择超时，请重试" end
        render()
    end, 60000)
    if not accepted then pickerError = "无法打开文件选择器"; render() end
end

local function resourceSettings(width)
    local pickers = {
        button("添加文件", function() pickAsset(false) end, { key = "import:file", glyph = "file", enabled = not libraryBusy() }),
        button("添加目录", function() pickAsset(true) end, { key = "import:folder", glyph = "folder", enabled = not libraryBusy() }),
    }
    local pathControls = {
        ui.input({ key = "import-path:" .. tostring(importRevision), value = importPath, placeholder = "文件或目录路径…",
            flexGrow = 1, controlSize = "md", enabled = not libraryBusy(), onChange = function(value) importPath = value end,
            onSubmit = function(value) importAsset(value) end }),
        button("添加", function() importAsset(importPath) end,
            { key = "import:submit", variant = "primary", enabled = not libraryBusy() }),
    }
    local rows = {}
    if width >= 640 then
        for _, node in ipairs(pathControls) do table.insert(pickers, node) end
        table.insert(rows, ui.row({ key = "import-controls", gap = 8, align = "center" }, pickers))
    else
        table.insert(rows, ui.column({ key = "import-controls", gap = 8 }, {
            ui.row({ gap = 8 }, pickers), ui.row({ gap = 8 }, pathControls),
        }))
    end
    if pickerError then table.insert(rows, label(pickerError, { fontSize = 12, color = "error", maxLines = 2 })) end
    local content = {}
    local configured, automatic, sources, seen = {}, {}, {}, {}
    for _, path in ipairs(data.configuredSources or {}) do configured[path] = true end
    for _, path in ipairs(data.automaticSources or {}) do automatic[path] = true end
    for _, paths in ipairs({ library().sources, data.configuredSources or {}, data.automaticSources or {} }) do
        for _, path in ipairs(paths) do
            if not seen[path] then table.insert(sources, path); seen[path] = true end
        end
    end
    if removingSource and not seen[removingSource] then removingSource = nil end
    local sourceRows = { label("壁纸来源", { fontSize = 14, fontWeight = "bold" }) }
    if #sources == 0 then table.insert(sourceRows, caption("尚未添加来源")) end
    for _, path in ipairs(sources) do
        local confirming = removingSource == path
        local actions = {}
        if automatic[path] then table.insert(actions, caption("自动"))
        elseif configured[path] then table.insert(actions, caption("配置")) end
        table.insert(actions, icon(confirming and "x" or "trash", confirming and "取消移除" or "移除来源", function()
            if removingSource == path then removingSource = nil else removingSource = path end
            render()
        end, { key = "remove-source:" .. path, enabled = not libraryBusy() }))
        local entry = { ui.row({ gap = 10, align = "center" }, {
            ui.glyph({ name = "photo", size = 18, color = "on_surface_variant" }),
            ui.column({ flexGrow = 1, gap = 4 }, {
                label(automatic[path] and "Wallpaper Engine" or path:gsub("/+$", ""):match("([^/]+)$") or path,
                    { fontSize = 14, fontWeight = "medium", maxLines = 1 }),
                label(path, { fontSize = 12, color = "on_surface_variant", maxLines = 2 }),
            }),
            ui.row({ gap = 8, align = "center" }, actions),
        }) }
        if confirming then
            table.insert(entry, ui.row({ key = "source-confirmation:" .. path, gap = 8, align = "center", paddingH = 28 }, {
                label("停止扫描此来源，保留原文件", { flexGrow = 1, fontSize = 12, color = "on_surface_variant", maxLines = 2 }),
                button("取消", function() removingSource = nil; render() end,
                    { key = "cancel-source:" .. path, variant = "ghost" }),
                button("移除", function()
                    if removingSource ~= path or libraryBusy() then return end
                    removingSource = nil
                    editLibrary({ action = "remove_source", path = path })
                end, { key = "confirm-remove-source:" .. path, variant = "destructive", enabled = not libraryBusy() }),
            }))
        end
        table.insert(sourceRows, ui.column({ key = "source:" .. path, paddingV = 6, gap = 6 }, entry))
    end
    table.insert(content, ui.column({ key = "wallpaper-sources", padding = 12, gap = 4,
        fill = "surface_variant/0.16", radius = 10 }, sourceRows))
    if #library().hidden > 0 then
        local hiddenRows = { label("已隐藏", { fontSize = 14, fontWeight = "bold" }) }
        for _, id in ipairs(library().hidden) do
            table.insert(hiddenRows, ui.row({ gap = 10, align = "center", paddingV = 6 }, {
                preview(findAsset(id), 48, 32),
                label(assetName(id), { flexGrow = 1, maxLines = 1, fontSize = 14 }),
                button("取消隐藏", function() editLibrary({ action = "hide", asset_id = id, hidden = false }) end,
                    { key = "restore:" .. id, variant = "ghost", enabled = not libraryBusy() }),
            }))
        end
        table.insert(content, ui.column({ key = "hidden-wallpapers", padding = 12, gap = 4,
            fill = "surface_variant/0.16", radius = 10 }, hiddenRows))
    end
    table.insert(rows, ui.scroll({ key = "resource-list", flexGrow = 1, gap = 12 }, content))
    return ui.column({ key = "resources", flexGrow = 1, gap = 12 }, rows)
end

local function header(width, compact, inlineDetails, layout)
    local rows, tools = {}, {}
    local browsing = page == "library" and not inlineDetails
    local close = icon("x", "关闭面板 · Esc", function() panel.close() end, { key = "header:close" })
    if compact then
        local ids, options = { "library", "static", "dynamic", "favorites" }, { "壁纸", "壁纸 / 静态", "壁纸 / 动态", "收藏" }
        for _, id in ipairs(playlistIds()) do table.insert(ids, id); table.insert(options, library().playlists[id].name) end
        table.insert(ids, "monitor"); table.insert(options, "设置")
        local current = page == "library" and (collection == "library" and kindFilter > 0 and (kindFilter == 1 and "static" or "dynamic") or collection) or page
        local navigation = {
            ui.select({ key = "compact-navigation", options = options,
                selectedIndex = (table.find(ids, current) or 1) - 1, flexGrow = 1, controlSize = "md",
                onChange = function(value)
                    local id = ids[(tonumber(value) or 0) + 1]
                    if id == "monitor" then page = id; render()
                    elseif id == "static" or id == "dynamic" then navigate("library", id == "static" and 1 or 2)
                    else navigate(id) end
                end }),
        }
        if page == "library" then
            table.insert(navigation, icon("playlist-add", "新建播放列表", function()
                page, selected, listForm = "library", nil, { name = "" }; render()
            end, { key = "compact-playlist:new", enabled = not libraryBusy() }))
        end
        table.insert(navigation, close)
        table.insert(rows, ui.row({ key = "compact-nav", gap = 8, align = "center" }, navigation))
    end
    if page == "monitor" then
        table.insert(tools, ui.row({ key = "settings-tabs", gap = 6, flexGrow = 1 }, {
            button("播放", function() settingsTab = 0; render() end,
                { key = "settings:playback", selected = settingsTab == 0, variant = "ghost" }),
            button("壁纸管理", function() settingsTab = 1; render() end,
                { key = "settings:resources", selected = settingsTab == 1, variant = "ghost" }),
        }))
    end
    if browsing then
        table.insert(tools, ui.input({ key = "search:" .. tostring(searchRevision), value = query, placeholder = "搜索…",
            flexGrow = 1, controlSize = "md", focus = focusSearch,
            onChange = function(value) query, gridPage = value, 1; scheduleRender() end }))
    end
    local selectors = browsing and libraryTools(width, layout) or {}
    local singleRow = width >= 620
    if singleRow then for _, node in ipairs(selectors) do table.insert(tools, node) end end
    if browsing or page == "monitor" and settingsTab == 1 then
        table.insert(tools, icon("refresh", "刷新图库 · Ctrl+R", function() request("refresh") end,
            { key = "header:refresh", enabled = not data.scanning }))
    end
    if browsing then
        table.insert(tools, icon("plus", "添加壁纸", function() page, settingsTab = "monitor", 1; render() end, { key = "header:import" }))
    end
    if not compact then
        if #tools == 0 then table.insert(tools, ui.spacer({ flexGrow = 1 })) end
        table.insert(tools, close)
    end
    if #tools > 0 then table.insert(rows, ui.row({ key = "toolbar", gap = 6, align = "center" }, tools)) end
    if not singleRow and #selectors > 0 then
        table.insert(rows, ui.row({ key = "toolbar-filters", gap = 6, align = "center" }, selectors))
    end
    return ui.column({ key = "header", gap = 8 }, rows)
end

local function footer(compact, width)
    local view = data.snapshot.outputs[screen]
    local rotation = data.snapshot.rotations and data.snapshot.rotations[screen]
    local left, right = {}, { statusChip(view) }
    local names = outputNames()
    if compact and #names > 1 then
        table.insert(left, ui.select({ key = "footer-output", options = names, width = 128,
            selectedIndex = (table.find(names, screen) or 1) - 1, controlSize = "sm",
            enabled = #names > 0, onChange = function(value) chooseScreen(names[(tonumber(value) or 0) + 1]) end }))
    else
        table.insert(left, preview(findAsset(view and view.current), 64, 36))
        table.insert(left, ui.column({ key = "output-info", gap = 4, flexGrow = 1 }, {
            label(screen or "无显示器", { fontWeight = "medium", fontSize = 14, maxLines = 1 }),
            rotation and rotation.settings.enabled and caption("轮播中") or nil,
        }))
    end
    if rotation and rotation.settings.enabled then
        table.insert(right, icon("player-skip-forward", "轮播下一张", function() request("next", screen) end,
            { key = "footer-next", enabled = not busy(screen) }))
    end
    if view and view.current and view.kind ~= "image" then
        table.insert(right, icon((view.mute or view.effective_mute) and "volume-off" or "volume",
            view.mute and "取消静音" or view.effective_mute and "静音 · 自动静音中" or "静音", function()
                request("playback", screen, { patch = { mute = not view.mute } })
            end, { key = "footer-mute", selected = view.mute, enabled = not busy(screen) }))
        table.insert(right, icon(view.paused and "player-play" or "player-pause", view.paused and "继续播放" or "暂停播放", function()
            request(view.paused and "resume" or "pause", screen)
        end, { key = "footer-pause", enabled = not busy(screen) }))
    end
    local current = label(assetName(view and view.current), {
        key = "current-info", maxLines = 1, fontWeight = "medium", textAlign = "center", flexGrow = 1,
    })
    local stacked = width < 620
    -- Equal side widths keep the title centered as playback controls appear or disappear.
    local sideWidth = not stacked and 240 or nil
    local controls = ui.row({ key = "footer-controls", gap = 8, align = "center" }, {
        ui.row({ key = "footer-left", width = sideWidth, gap = 8, align = "center" }, left),
        stacked and ui.spacer({ flexGrow = 1 }) or current,
        ui.row({ key = "footer-right", width = sideWidth, gap = 8, align = "center", justify = "end" }, right),
    })
    local children = { controls }
    if stacked then table.insert(children, 1, ui.row({ key = "footer-title", align = "center" }, { current })) end
    return ui.column({ key = "footer", paddingH = 12, paddingV = 7, gap = 6,
        fill = "surface_variant/0.16", radius = 12 }, children)
end

render = function()
    if not opened then return end
    if page ~= "monitor" or settingsTab ~= 1 or not data.connected then removingSource = nil end
    pendingRender = false
    panel.setNeedsFrameTick(false)
    indexAssets()
    indexLibrary()
    if collection ~= "library" and collection ~= "favorites" and not library().playlists[collection] then
        collection, selected, deletingList = "library", nil, nil
    end
    if addingTo and not library().playlists[addingTo] then addingTo = nil end
    local names = outputNames()
    if data.connected and not data.snapshot.outputs[screen] then
        local focused = noctalia.focusedOutputName()
        screen = table.find(names, focused) and focused or names[1]
        target = nil
    end
    if target ~= "*" and target and not data.snapshot.outputs[target] then target = nil end
    local outputs = noctalia.outputs()
    local focusedOutput = nil
    for _, output in ipairs(outputs) do
        if output.name == noctalia.focusedOutputName() or output.focused then focusedOutput = output; break end
    end
    focusedOutput = focusedOutput or outputs[1]
    -- API 24 has no content-size callback. Allow room for the host's frame.
    local width = math.min(1240, focusedOutput and (focusedOutput.width - 24) or 1240) - 32
    local compact = width < 780
    local sideWidth = compact and 0 or 156
    local workspaceWidth = math.max(280, width - 16 - sideWidth - (compact and 0 or 10) - 20)
    local asset = selected and findAsset(selected)
    local inlineDetails = asset and page == "library" and workspaceWidth < 650
    local detailWidth = 316
    local contentWidth = workspaceWidth - (asset and page == "library" and not inlineDetails and (detailWidth + 12) or 0)
    local layout = page == "library" and not inlineDetails and galleryLayout(contentWidth) or nil
    local body, workspace = {}, { header(contentWidth, compact, inlineDetails, layout) }
    if not compact then table.insert(body, sidebar(sideWidth)) end
    if data.error then
        table.insert(workspace, ui.row({ padding = 12, gap = 8, fill = "error/0.08", radius = 10 }, {
            ui.glyph({ name = "alert-circle", size = 18, color = "error" }),
            label(data.error, { fontSize = 12, color = "error", maxLines = 2, flexGrow = 1 }),
        }))
    end
    local libraryOperation = data.libraryOperation
    if libraryOperation and libraryOperation.error then
        table.insert(workspace, ui.row({ key = "library-error", padding = 8, gap = 8, align = "center", fill = "error/0.08", radius = 8 }, {
            label(libraryOperation.error, { fontSize = 12, color = "error", flexGrow = 1, maxLines = 2 }),
            button("重试", function()
                local edit = libraryOperation.request.edit
                if edit.action == "remove_source" then
                    page, settingsTab, removingSource = "monitor", 1, edit.path
                    render()
                else request("library-edit", nil, libraryOperation.request) end
            end,
                { key = "library:retry", variant = "default", enabled = not libraryBusy() }),
        }))
    end
    if page == "monitor" then
        table.insert(workspace, settingsTab == 1 and resourceSettings(contentWidth) or monitorSettings(contentWidth))
    elseif inlineDetails then table.insert(workspace, details(asset, workspaceWidth, true))
    else
        table.insert(workspace, gallery(layout))
    end
    table.insert(body, ui.column({ key = "workspace", padding = 10, gap = 12, flexGrow = 1 }, workspace))
    if asset and page == "library" and not inlineDetails then table.insert(body, details(asset, detailWidth)) end
    panel.render(ui.column({ key = "wallpaper", paddingH = 6, paddingV = 2, gap = 4, flexGrow = 1 }, {
        ui.row({ key = "body", gap = 10, flexGrow = 1 }, body), footer(compact, width - 16),
    }))
end

noctalia.state.watch("wallpaper", function(value)
    if type(value) ~= "table" then return end
    data = value
    local operation = data.libraryOperation
    if operation and operation.request and not operation.busy and not operation.error and handledLibrary ~= operation.request.id then
        handledLibrary = operation.request.id
        if operation.request.edit.action == "create_playlist" or operation.request.edit.action == "rename_playlist" then listForm = nil end
        if operation.created and handledCreation ~= operation.created then
            handledCreation = operation.created
            page, collection, selected = "library", operation.created, nil
            query, kindFilter, sourceFilter, gridPage = "", 0, 0, 1
            searchRevision += 1
        elseif operation.request.edit.action == "import" then
            importPath = ""; importRevision += 1
        elseif operation.request.edit.action == "rename_asset" and titleEdit and titleEdit.asset == operation.request.edit.asset_id then
            titleEdit = nil
        end
    end
    for output, result in pairs(data.operations) do
        local message = result.request
        if message and not result.busy and not result.error and handledOperations[output] ~= message.id then
            handledOperations[output] = message.id
            if message.draftKey then
                local draft = propertyDrafts[message.draftKey]
                local completed = true
                if message.output == "*" then
                    for _, name in ipairs(outputNames()) do
                        local item = data.operations[name]
                        completed = completed and item and item.request.id == message.id and not item.busy and not item.error
                    end
                end
                if completed and draft and draft.version == message.draftVersion then
                    propertyDrafts[message.draftKey] = nil
                    propertyRevisions[message.draftKey] = (propertyRevisions[message.draftKey] or 0) + 1
                end
            end
            if message.action == "rotation" then
                rotationDrafts[output .. "|" .. message.settings.source] = nil
            end
        end
    end
    render()
end)

function onOpen()
    opened = true
    data = noctalia.state.get("wallpaper") or data
    screen = screen or noctalia.focusedOutputName()
    render()
    request("refresh", nil, { ifStale = true })
    local asset = selected and findAsset(selected)
    if asset then queryProperties(asset) end
end

function onClose()
    opened, pendingRender, focusSearch = false, false, false
    removingSource = nil
    panel.setNeedsFrameTick(false)
end

function onFrameTick()
    if pendingRender then render() end
    panel.setNeedsFrameTick(false)
end

function onKey(chord, pressed)
    if not pressed then return false end
    if chord == "ctrl+r" then request("refresh"); return true end
    if chord == "ctrl+f" then
        page, selected, focusSearch = "library", nil, true
        searchRevision += 1
        panel.setNeedsFrameTick(false)
        render()
        return true
    end
    return false
end
