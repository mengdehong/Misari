local function isChineseText(text)
    local cjk, latin = 0, 0
    for _, code in utf8.codes(text) do
        if code >= 0x3400 and code <= 0x9FFF or code >= 0x3040 and code <= 0x30FF then cjk += 1
        elseif code >= 65 and code <= 90 or code >= 97 and code <= 122 then latin += 1 end
    end
    -- A Han character in an emoticon (e.g. (*´∀人)) does not make English prose Chinese.
    return cjk > 0 and cjk * 2 >= latin
end

local function trimText(text)
    local first = text:find("%S")
    if not first then return "" end
    local last = #text - text:reverse():find("%S") + 1
    return text:sub(first, last)
end

local function plainText(text)
    text = tostring(text)
    -- Luau invokes the host's CPU-budget interrupt for pattern matches.
    -- Skip absent markup/entities instead of scanning every label repeatedly.
    if text:find("<", 1, true) then
        text = text:gsub("<[bB][rR]%s*/?%s*>", "\n")
            :gsub("</[pP]>", "\n"):gsub("</[hH]%d>", "\n"):gsub("<[^>]*>", "")
    end
    if text:find("&", 1, true) then
        text = text:gsub("&nbsp;", " "):gsub("&amp;", "&"):gsub("&quot;", '"')
            :gsub("&#39;", "'"):gsub("&lt;", "<"):gsub("&gt;", ">")
    end
    return trimText(text)
end

local function localizedText(text, chineseProperties)
    text = plainText(text)
    local slash = text:find("/", 1, true)
    if slash and slash < #text and not text:find("://", 1, true) then
        local first, second = trimText(text:sub(1, slash - 1)), trimText(text:sub(slash + 1))
        if second ~= "" and isChineseText(first) ~= isChineseText(second) then
            text = isChineseText(first) == chineseProperties and first or second
        end
    end
    -- Author notes often put translations in separate HTML paragraphs.
    if not text:find("\n", 1, true) then return text end
    local native, foreign = {}, {}
    for line in text:gmatch("[^\n]+") do
        table.insert(isChineseText(line) and native or foreign, line)
    end
    if #native > 0 and #foreign > 0 then text = table.concat(chineseProperties and native or foreign, "\n") end
    return trimText(text)
end

return localizedText
