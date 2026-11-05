-- AsuraScans API/Astro contract adapted from TachiKindle (Apache-2.0).
-- See docs/licensing.md in the repository for provenance.
local JSON = require("json")
local util = require("util")
local urlutil = require("socket.url")
local Http = require("kindleturbohttp")

local Source = { id = "asurascans", name = "AsuraScans", site = "https://asurascans.com", api = "https://api.asurascans.com/api" }

function Source.unwrap(value)
    if type(value) ~= "table" then return value end
    if #value == 2 and type(value[1]) == "number" then return Source.unwrap(value[2]) end
    local out = {}
    for key, item in pairs(value) do out[key] = Source.unwrap(item) end
    return out
end

function Source.props(html, key)
    for raw in html:gmatch('props%s*=%s*"(.-)"') do
        local ok, value = pcall(JSON.decode, util.htmlEntitiesToUtf8(raw))
        if ok and type(value) == "table" then
            value = Source.unwrap(value)
            if type(value) == "table" and type(value[key]) == "table" then return value end
        end
    end
    return nil, "AsuraScans: dados '" .. key .. "' ausentes. O site pode ter mudado ou bloqueado a conexão."
end

function Source.get(url, image)
    return Http.get(url, Source.site, 1, image)
end

function Source.json(path)
    local body, err = Source.get(Source.api .. path)
    if not body then return nil, err end
    local ok, value = pcall(JSON.decode, body)
    if not ok or type(value) ~= "table" then return nil, "Resposta inválida da API AsuraScans" end
    return value
end

function Source.search(query, page)
    local response, err = Source.json("/series?offset=" .. ((page - 1) * 20) .. "&limit=20&search=" .. urlutil.escape(query or ""))
    if not response then return nil, err end
    if type(response.data) ~= "table" then return nil, "AsuraScans: catálogo inválido" end
    for _, item in ipairs(response.data) do
        if type(item) ~= "table" or type(item.title) ~= "string" or type(item.slug) ~= "string" or #item.slug > 120 or not item.slug:match("^[%w%-]+$") then
            return nil, "AsuraScans: título ou slug inválido"
        end
    end
    return response.data, nil, type(response.meta) == "table" and response.meta.has_more == true
end

function Source.chapters(slug)
    if type(slug) ~= "string" or #slug > 120 or not slug:match("^[%w%-]+$") then return nil, "Slug inválido" end
    local details, err = Source.json("/series/" .. slug)
    if not details then return nil, err end
    if type(details.data) == "table" then details = details.data end
    details = details.series or details
    if type(details) ~= "table" then return nil, "AsuraScans: dados da série inválidos" end
    if type(details.title) ~= "string" or details.title == "" then return nil, "AsuraScans: título ausente" end
    if type(details.public_url) ~= "string" or not details.public_url:match("^/comics/[%w%-]+$") then
        return nil, "AsuraScans: caminho da série inválido"
    end
    local html
    html, err = Source.get(Source.site .. details.public_url)
    if not html then return nil, err end
    local value
    value, err = Source.props(html, "chapters")
    if not value then return nil, err end
    for _, chapter in ipairs(value.chapters) do
        if type(chapter) ~= "table" or type(chapter.number) ~= "number" or chapter.number < 0 or chapter.number == math.huge then
            return nil, "Número de capítulo inválido"
        end
        if type(chapter.title) ~= "string" then chapter.title = "" end
    end
    table.sort(value.chapters, function(a, b) return a.number < b.number end)
    return value.chapters, nil, details
end

function Source.locked(chapter)
    return chapter.is_locked == true or chapter.is_premium == true
end

function Source.pages(public_path, number)
    local html, err = Source.get(Source.site .. public_path .. "/chapter/" .. tostring(number))
    if not html then return nil, err end
    local value
    value, err = Source.props(html, "pages")
    if not value then return nil, err end
    if #value.pages == 0 then return nil, "Capítulo sem páginas públicas (bloqueado ou indisponível)" end
    local pages = {}
    for _, page in ipairs(value.pages) do
        if type(page) ~= "table" then return nil, "Dados de página inválidos" end
        if type(page.tiles) == "table" and #page.tiles > 0 then return nil, "Imagens embaralhadas ainda não suportadas" end
        if type(page.url) ~= "string" or not page.url:match("^https://") or page.url:find("#", 1, true) then
            return nil, "URL de página não suportada"
        end
        pages[#pages + 1] = page.url
    end
    return pages
end

Source.imageExtension = Http.imageExtension

function Source.chapterURL(details, chapter)
    return Source.site .. "/series/" .. details.slug .. "/chapter/" .. tostring(chapter.number)
end

return Source
