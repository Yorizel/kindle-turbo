local htmlparser = require("htmlparser")
local util = require("util")
local urlutil = require("socket.url")
local Http = require("kindleturbohttp")
local Source = { id = "weebcentral", name = "WeebCentral", site = "https://weebcentral.com" }

local function document(html)
    -- KOReader defaults to 1000 parser iterations and returns a partial tree.
    -- A byte-bound covers tag/attribute loops for the full, response-size-limited catalog.
    return htmlparser.parse(html, math.max(1000, #html + 1))
end

local function decoded(value)
    return type(value) == "string" and util.htmlEntitiesToUtf8(value) or ""
end

local function text(node)
    return util.htmlToPlainText(node:getcontent()):gsub("%s+", " "):match("^%s*(.-)%s*$")
end

local function identifier(value, kind)
    if type(value) ~= "string" then return nil end
    local path = value
    if path:sub(1, #Source.site) == Source.site then path = path:sub(#Source.site + 1) end
    local prefix = "/" .. kind .. "/"
    if path:sub(1, #prefix) == prefix then path = path:sub(#prefix + 1) end
    local id = path:match("^([^/?#]+)")
    if id and #id == 26 and id:match("^[A-Za-z0-9]+$") then return id end
end

local function linked_id(value, kind)
    if type(value) ~= "string" then return nil end
    local relative = "/" .. kind .. "/"
    if value:sub(1, #relative) ~= relative and value:sub(1, #Source.site + #relative) ~= Source.site .. relative then return nil end
    return identifier(value, kind)
end

function Source.get(url, image)
    return Http.get(url, Source.site, 2, image)
end

function Source.parseSearch(html)
    local doc = document(html)
    local items, seen = {}, {}
    for _, node in ipairs(doc:select("article > section > a")) do
        local id = linked_id(decoded(node.attributes.href), "series")
        if id and not seen[id] then
            local title
            for _, value in ipairs(node:select("div:not([class])")) do
                local value_text = text(value)
                if value_text ~= "" then title = value_text; break end
            end
            if not title then
                local image = node:select("img")[1]
                if image then title = decoded(image.attributes.alt):gsub(" cover$", "") end
            end
            if not title or title == "" then return nil, "WeebCentral: título ausente" end
            items[#items + 1] = { slug = id, title = title }
            seen[id] = true
        end
    end
    local more = false
    for _, node in ipairs(doc:select("button[hx-get]")) do
        local url = decoded(node.attributes["hx-get"])
        if url:find("/search/data?", 1, true) and url:find("offset=", 1, true) then more = true end
    end
    if #items == 0 and html:match("%S") and not html:lower():find("no results", 1, true) then
        return nil, "WeebCentral: resposta de busca desconhecida ou bloqueada"
    end
    return items, nil, more
end

function Source.search(query, page)
    local path = "/search/data?text=" .. urlutil.escape(query or "") .. "&limit=32&offset=" .. ((page - 1) * 32) .. "&display_mode=Full+Display"
    if not query or query == "" then path = path .. "&sort=Popularity" end
    local body, err = Source.get(Source.site .. path)
    if not body then return nil, err end
    return Source.parseSearch(body)
end

function Source.parseChapters(html)
    local doc = document(html)
    local chapters, seen = {}, {}
    for _, node in ipairs(doc:select("div[x-data] > a")) do
        local id = linked_id(decoded(node.attributes.href), "chapters")
        if id and not seen[id] then
            local title
            for _, label in ipairs(node:select("span.flex > span")) do
                local label_text = text(label)
                if label_text ~= "" then title = label_text; break end
            end
            if not title then return nil, "WeebCentral: rótulo de capítulo ausente" end
            local lower = title:lower()
            local number = tonumber(lower:match("^chapter (%S+)") or lower:match("^episode (%S+)"))
            if number and (number ~= number or number < 0 or number == math.huge) then number = nil end
            chapters[#chapters + 1] = { id = id, title = title, label = title, number = number, url = Source.site .. "/chapters/" .. id }
            seen[id] = true
        end
    end
    if #chapters == 0 then return nil, "WeebCentral: lista de capítulos vazia ou bloqueada" end
    local ordered = {}
    for index = #chapters, 1, -1 do ordered[#ordered + 1] = chapters[index] end
    return ordered
end

function Source.chapters(input)
    local id = identifier(input, "series")
    if not id then return nil, "WeebCentral: ID de série inválido" end
    local url = Source.site .. "/series/" .. id
    local body, err = Source.get(url)
    if not body then return nil, err end
    local title
    for _, node in ipairs(document(body):select("section[x-data] h1")) do
        local value = text(node)
        if value ~= "" then title = value; break end
    end
    if not title then return nil, "WeebCentral: título da série ausente" end
    body, err = Source.get(url .. "/full-chapter-list")
    if not body then return nil, err end
    local chapters
    chapters, err = Source.parseChapters(body)
    if not chapters then return nil, err end
    return chapters, nil, { title = title, slug = id, public_url = url }
end

function Source.parsePages(html)
    local pages = {}
    for _, image in ipairs(document(html):select("section#chapter-images img")) do
        local url = decoded(image.attributes.src)
        if not url:match("^https://") or url:find("#", 1, true) then return nil, "WeebCentral: URL de imagem inválida" end
        pages[#pages + 1] = url
    end
    if #pages == 0 then return nil, "WeebCentral: capítulo sem imagens ou resposta bloqueada" end
    return pages
end

function Source.pages(_, _, chapter)
    local id = chapter and linked_id(chapter.url, "chapters")
    if not id then return nil, "WeebCentral: ID de capítulo inválido" end
    local body, err = Source.get(Source.site .. "/chapters/" .. id .. "/images?is_prev=False&reading_style=long_strip")
    if not body then return nil, err end
    return Source.parsePages(body)
end

function Source.chapterURL(_, chapter)
    return chapter.url
end

function Source.locked(_)
    return false
end

Source.imageExtension = Http.imageExtension
return Source
