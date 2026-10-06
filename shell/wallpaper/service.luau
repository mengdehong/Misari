-- Business data stays in wallpaperd. Only this entry owns wallpaperd processes.
local data = {
    connected = false,
    assets = {},
    assetsRevision = 0,
    snapshot = { outputs = {} },
    operations = {},
    scanning = false,
    error = nil,
    library = { sources = {}, favorites = {}, playlists = {}, hidden = {} },
    libraryRevision = 0,
    libraryOperation = {},
    configuredSources = {},
    automaticSources = {},
    properties = {},
    backgrounds = {},
}
local streamStarted = false
local visibility = {}
local libraryRevision, propertyRevision = 0, 0
local libraryJson, lastSnapshotLine, lastScanMs = nil, nil, nil
local transitions = { fade = true, disc = true, honeycomb = true, spiral = true, stripes = true }

local function publish()
    noctalia.state.set("wallpaper", data)
end

local function setLibrary(value)
    local encoded = noctalia.json.encode(value)
    if encoded ~= libraryJson then
        data.library, libraryJson = value, encoded
        data.libraryRevision += 1
    end
end

local function syncSurfaces()
    for _, output in ipairs(noctalia.outputs()) do
        local view = data.snapshot.outputs[output.name]
        local enabled = not (data.connected and view and view.current and view.renderer == "running")
        if visibility[output.name] ~= enabled then
            noctalia.setWallpaperEnabled(output.name, enabled)
            visibility[output.name] = enabled
        end
    end
end

local function decode(raw)
    local ok, value = pcall(noctalia.json.decode, raw)
    if ok and type(value) == "table" then
        return value
    end
    return nil
end

local function resultError(result)
    if result.timedOut then
        return "操作超时，请检查显示器状态后重试"
    elseif result.stdoutTruncated then
        return "wallpaperd 返回内容过大"
    elseif result.exitCode ~= 0 then
        return result.stderr ~= "" and result.stderr or "wallpaperd 命令失败"
    end
    return nil
end

local function replyError(result, reply)
    return resultError(result) or (not reply and "无法读取操作结果")
        or (reply.ok ~= true and (reply.error and reply.error.message or "操作失败")) or nil
end

local function syncBackgrounds()
    if not data.connected or noctalia.getConfig("sync_backgrounds") == false then return end
    for name, view in pairs(data.snapshot.outputs) do
        if not view.current or view.renderer ~= "running" then
            data.backgrounds[name] = nil
        elseif view.revision and not view.pending then
            local background = data.backgrounds[name] or {}
            data.backgrounds[name] = background
            if background.desiredRevision ~= view.revision then
                background.desiredRevision = view.revision
                -- ponytail: native WE accepts properties before rendering them.
                -- Let an existing wallpaper settle for five seconds; replace
                -- this delay when the engine exposes a property/frame revision.
                background.readyAt = background.current == view.current and noctalia.nowMs() + 5000 or 0
            end
            if not background.busy and background.revision ~= view.revision
                and noctalia.nowMs() >= background.readyAt then
                local output, revision, current = name, view.revision, view.current
                background.busy = true
                local accepted = noctalia.runAsync({ "wallpaperd", "snapshot", "--output", output,
                    "--revision", tostring(revision) }, function(result)
                    -- Reconnects and hotplug discard requests from the previous context.
                    if data.backgrounds[output] ~= background then return end
                    background.busy = false
                    local latest = data.snapshot.outputs[output]
                    if not data.connected or not latest or latest.current ~= current
                        or latest.revision ~= revision or latest.pending then return end
                    local reply = decode(result.stdout)
                    background.error = replyError(result, reply)
                    if not background.error and (type(reply.path) ~= "string"
                        or reply.output ~= output or reply.revision ~= revision or reply.current ~= current) then
                        background.error = "背景帧与当前壁纸不一致"
                    end
                    if not background.error and noctalia.getConfig("sync_backgrounds") ~= false then
                        noctalia.setWallpaper(output, reply.path)
                        background.revision, background.path, background.current = revision, reply.path, current
                    end
                    publish()
                end, 60000)
                if not accepted then background.busy = false; background.error = "无法抽取壁纸背景"; publish() end
            end
        end
    end
end

local function loadLibrary()
    local revision = libraryRevision
    noctalia.runAsync({ "wallpaperd", "library" }, function(result)
        local reply = decode(result.stdout)
        local error = replyError(result, reply)
        if not error then
            if revision == libraryRevision then setLibrary(reply.library) end
            data.configuredSources = reply.configured_sources or {}
            data.automaticSources = reply.automatic_sources or {}
        else data.error = error end
        publish()
    end, 60000)
end

local function refresh(ifStale)
    if data.scanning then
        return
    end
    if ifStale and lastScanMs and not data.error and noctalia.nowMs() - lastScanMs < 60000 then return end
    data.scanning = true
    publish()
    local accepted = noctalia.runAsync({ "wallpaperd", "list", "--thumbnails" }, function(result)
        data.scanning = false
        local reply = decode(result.stdout)
        data.error = resultError(result)
        if not data.error and reply and reply.ok == true and type(reply.assets) == "table" then
            data.assets = reply.assets
            data.assetsRevision += 1
            lastScanMs = noctalia.nowMs()
            loadLibrary()
        elseif not data.error then
            data.error = "无法读取素材目录"
        end
        publish()
    end, 60000)
    if not accepted then
        data.scanning = false
        data.error = "无法启动 wallpaperd，请确认已安装并加入 PATH"
        publish()
    end
end

local function onLine(line)
    if data.connected and line == lastSnapshotLine then return end
    local snapshot = decode(line)
    if snapshot and snapshot.connection == "closed" then
        data.connected = false
        data.backgrounds = {}
        lastSnapshotLine = nil
        data.error = "wallpaperd 未连接，正在重连"
        syncSurfaces()
        publish()
    elseif snapshot and snapshot.api == 1 and type(snapshot.outputs) == "table" then
        local reconnecting = not data.connected
        data.snapshot = snapshot
        lastSnapshotLine = line
        if snapshot.library then setLibrary(snapshot.library); libraryRevision += 1 end
        data.connected = true
        if reconnecting then
            data.error = nil
            -- A new daemon may have different startup resource directories.
            data.operations = {}
            data.properties = {}
            data.libraryOperation = {}
            data.backgrounds = {}
            refresh()
        end
        syncSurfaces()
        syncBackgrounds()
        publish()
    else
        data.error = "无法读取 wallpaperd 状态（需要 JSON api: 1）"
        publish()
    end
end

local function propertyJson(request)
    local fields = {}
    for key, value in pairs(request.properties or {}) do
        table.insert(fields, noctalia.json.encode(key) .. ":" .. noctalia.json.encode(value))
    end
    for _, key in ipairs(request.reset or {}) do
        if type(key) ~= "string" then return nil end
        table.insert(fields, noctalia.json.encode(key) .. ":null")
    end
    return "{" .. table.concat(fields, ",") .. "}"
end

local function getProperties(request)
    if type(request.asset) ~= "string" then return end
    local previous = data.properties[request.asset]
    propertyRevision += 1
    local revision = propertyRevision
    local keepReply = previous and (previous.context == request.context or previous.context == nil)
    local entry = { loading = true, context = request.context, reply = keepReply and previous.reply,
        revision = keepReply and previous.revision or 0 }
    data.properties[request.asset] = entry
    publish()
    local argv = { "wallpaperd", "properties", request.asset }
    if request.properties or request.reset then
        table.insert(argv, "--properties")
        table.insert(argv, propertyJson(request))
    end
    local accepted = noctalia.runAsync(argv, function(result)
        if data.properties[request.asset] ~= entry then return end
        local reply = decode(result.stdout)
        entry.loading, entry.error = false, replyError(result, reply)
        if not entry.error then entry.reply = reply; entry.revision = revision end
        publish()
    end, 60000)
    if not accepted then entry.loading = false; entry.error = "无法查询属性"; publish() end
end

local function editLibrary(request)
    if data.libraryOperation.busy or type(request.edit) ~= "table" then return end
    local edit = request.edit
    local encoded = noctalia.json.encode(edit)
    -- The host serializes an empty Lua table as an object, not a JSON array.
    if edit.action == "playlist_members" and type(edit.members) == "table" and #edit.members == 0 then
        encoded = '{"action":"playlist_members","id":' .. noctalia.json.encode(edit.id) .. ',"members":[]}'
    end
    local operation = { busy = true, request = request }
    local revision = libraryRevision
    data.libraryOperation = operation
    publish()
    local accepted = noctalia.runAsync({ "wallpaperd", "library-edit", encoded }, function(result)
        if data.libraryOperation ~= operation then return end
        local reply = decode(result.stdout)
        operation.busy, operation.error = false, replyError(result, reply)
        if not operation.error then
            if revision == libraryRevision then setLibrary(reply.library) end
            operation.created = reply.created
            if edit.action == "import" or edit.action == "remove_source" then refresh() end
        end
        publish()
    end, 60000)
    if not accepted then operation.busy = false; operation.error = "无法更新素材库"; publish() end
end

local function startStream()
    if streamStarted then
        return
    end
    -- runStream has no exit callback. One supervised shell reports disconnects
    -- and reconnects; the host terminates its process group on entry teardown.
    streamStarted = noctalia.runStream(
        [[while :; do
    wallpaperd subscribe 2>/dev/null
    printf '%s\n' '{"connection":"closed"}'
    sleep 2
done]], onLine)
    if not streamStarted then
        data.error = "无法启动 wallpaperd 状态订阅"
        publish()
    end
end

local function command(request, output)
    local action = request.action
    local argv = { "wallpaperd", action == "apply" and "set" or action }
    if action == "apply" then
        if type(request.asset) ~= "string" or request.asset == "" then
            return nil
        end
        if request.fit ~= "cover" and request.fit ~= "contain" and request.fit ~= "stretch" then
            return nil
        end
        if not transitions[request.transition] then
            return nil
        end
        table.insert(argv, request.asset)
        table.insert(argv, "--fit")
        table.insert(argv, request.fit)
        table.insert(argv, "--transition")
        table.insert(argv, request.transition)
        if request.properties or request.reset then
            table.insert(argv, "--properties")
            table.insert(argv, propertyJson(request))
        end
    elseif action == "set-properties" then
        if type(request.asset) ~= "string" or type(request.properties or request.reset) ~= "table" then return nil end
        table.insert(argv, request.asset)
        table.insert(argv, propertyJson(request))
    elseif action == "rotation" then
        if type(request.settings) ~= "table" then return nil end
        table.insert(argv, noctalia.json.encode(request.settings))
    elseif action == "playback" then
        local patch = request.patch
        if type(patch) ~= "table" then
            return nil
        end
        local count = 0
        for _, key in ipairs({ "fps", "volume", "mute" }) do
            local value = patch[key]
            if value ~= nil then
                if key == "mute" then
                    if type(value) ~= "boolean" then return nil end
                elseif type(value) ~= "number" or value % 1 ~= 0
                    or value < (key == "fps" and 1 or 0) or value > (key == "fps" and 240 or 100) then
                    return nil
                end
                table.insert(argv, "--" .. key)
                table.insert(argv, tostring(value))
                count += 1
            end
        end
        if count == 0 then return nil end
    elseif action ~= "pause" and action ~= "resume" and action ~= "release" and action ~= "next" then
        return nil
    end
    table.insert(argv, "--output")
    table.insert(argv, output)
    return argv
end

local function execute(request, output)
    local previous = data.operations[output]
    if previous and previous.busy then
        return
    end
    local argv = command(request, output)
    if not argv then
        data.error = "无效的壁纸操作"
        publish()
        return
    end
    local operation = { busy = true, action = request.action, asset = request.asset, request = request }
    data.operations[output] = operation
    publish()
    local accepted = noctalia.runAsync(argv, function(result)
        -- Ignore an old command's completion after a daemon reconnect.
        if data.operations[output] ~= operation then return end
        operation.busy = false
        local reply = decode(result.stdout)
        operation.error = replyError(result, reply)
        if not operation.error and (request.action == "set-properties" or request.action == "apply")
            and request.asset and request.asset:sub(1, 3) == "we:" then
            getProperties({ asset = request.asset })
        end
        -- Never infer current from command success: only snapshots own reality.
        publish()
    end, 60000)
    if not accepted then
        operation.busy = false
        operation.error = "无法启动 wallpaperd 命令"
        publish()
    end
end

noctalia.state.watch("wallpaper-request", function(request)
    if type(request) ~= "table" then return end
    if request.action == "refresh" then
        refresh(request.ifStale)
        return
    end
    if not data.connected then return end
    if request.action == "properties" then getProperties(request); return end
    if request.action == "library-edit" then editLibrary(request); return end
    if request.output == "*" then
        local names = {}
        for name in pairs(data.snapshot.outputs) do table.insert(names, name) end
        table.sort(names)
        -- Explicit per-output commands preserve individual results on failure.
        for _, name in ipairs(names) do execute(request, name) end
    elseif type(request.output) == "string" and data.snapshot.outputs[request.output] then
        execute(request, request.output)
    end
end)

function update()
    startStream()
    syncBackgrounds()
end

function onOutputsChanged()
    visibility = {}
    data.backgrounds = {}
    syncSurfaces()
    syncBackgrounds()
end

function onIpc(event)
    if event == "refresh" then refresh() end
end

function onExit()
    -- wallpaperd keeps playing independently when this plugin is unloaded.
    -- Release only the Noctalia surface overrides owned by this service.
    for name in pairs(visibility) do noctalia.setWallpaperEnabled(name, true) end
end

noctalia.setUpdateInterval(5000)
publish()
startStream()
