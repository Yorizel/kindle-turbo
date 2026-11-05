local WidgetContainer = require("ui/widget/container/widgetcontainer")
local UIManager = require("ui/uimanager")
local Menu = require("ui/widget/menu")
local InputDialog = require("ui/widget/inputdialog")
local InfoMessage = require("ui/widget/infomessage")
local ButtonDialog = require("ui/widget/buttondialog")
local ConfirmBox = require("ui/widget/confirmbox")
local NetworkMgr = require("ui/network/manager")
local DataStorage = require("datastorage")
local LuaSettings = require("luasettings")
local ReaderUI = require("apps/reader/readerui")
local lfs = require("libs/libkoreader-lfs")
local util = require("util")
local JSON = require("json")
local natural = require("sort").natsort_cmp()
local PluginShare = require("pluginshare")
local Source = require("kindleturboasura")

local Turbo = WidgetContainer:extend{ name = "kindle_turbo", is_doc_only = false }

local function filename(value)
    local result = util.getSafeFilename(value:gsub("%c", "_"), nil, 100):gsub("^%.*", "")
    return result ~= "" and result or "serie"
end

function Turbo:init()
    self.settings = LuaSettings:open(DataStorage:getSettingsDir() .. "/kindle_turbo.lua")
    self.root = self.settings:readSetting("download_dir", DataStorage:getDataDir() .. "/kindle-turbo/downloads")
    self.ui.menu:registerToMainMenu(self)
end

function Turbo:message(text)
    UIManager:show(InfoMessage:new{ text = tostring(text) })
end

function Turbo:addToMainMenu(items)
    items.kindle_turbo = {
        text = "Kindle Turbo · AsuraScans",
        sorting_hint = "more_tools",
        sub_item_table = {
            { text = "Buscar manhwa", callback = function() self:searchDialog() end },
            { text = "Catálogo AsuraScans", callback = function() self:catalog("", 1) end },
            { text = "Downloads / ler offline", callback = function() self:library() end },
            { text = "Pasta dos downloads", callback = function() self:downloadDirectory() end },
        },
    }
end

function Turbo:showMenu(title, items)
    self.menus = self.menus or {}
    local menu
    menu = Menu:new{
        title = title, item_table = items, is_popout = false, is_borderless = true,
        title_bar_fm_style = true,
        close_callback = function() self.menus[menu] = nil; UIManager:close(menu) end,
    }
    self.menus[menu] = true
    UIManager:show(menu)
    return menu
end

function Turbo:searchDialog()
    local dialog
    dialog = InputDialog:new{
        title = "Buscar no AsuraScans", input = "",
        buttons = {{
            { text = "Cancelar", callback = function() UIManager:close(dialog) end },
            { text = "Buscar", is_enter_default = true, callback = function()
                local query = dialog:getInputText()
                UIManager:close(dialog)
                self:catalog(query, 1)
            end },
        }},
    }
    UIManager:show(dialog)
    dialog:onShowKeyboard()
end

function Turbo:catalog(query, page)
    NetworkMgr:runWhenConnected(function()
        local list, err, more = Source.search(query, page)
        if not list then self:message(err); return end
        local items = {}
        for _, manga in ipairs(list) do
            items[#items + 1] = { text = manga.title, callback = function() self:chapters(manga.slug) end }
        end
        local menu
        if page > 1 then
            items[#items + 1] = { text = "← Página anterior", callback = function() UIManager:close(menu); self:catalog(query, page - 1) end }
        end
        if more then
            items[#items + 1] = { text = "Próxima página →", callback = function() UIManager:close(menu); self:catalog(query, page + 1) end }
        end
        if #items == 0 then self:message("Nenhum resultado"); return end
        menu = self:showMenu("AsuraScans · página " .. page, items)
    end)
end

function Turbo:chapterPath(details, chapter)
    return self.root .. "/" .. filename(details.title) .. " [" .. details.slug .. "]/Capítulo " .. tostring(chapter.number) .. ".cbz"
end

function Turbo:chapterURL(details, chapter)
    return Source.site .. "/series/" .. details.slug .. "/chapter/" .. tostring(chapter.number)
end

function Turbo:archiveValid(path, expected_url)
    local ok, archive = pcall(require, "ffi/archiver")
    if not ok or not archive.Reader then return false end
    local reader = archive.Reader:new()
    if not reader:open(path) then return false end
    local count, manifest = 0, nil
    for entry in reader:iterate() do
        if entry.mode ~= "file" or entry.size <= 0 or entry.size > 64 * 1024 * 1024 then reader:close(); return false end
        if entry.path == "kindle-turbo.json" then
            if entry.size > 65536 then reader:close(); return false end
            local body = reader:extractToMemory(entry.index)
            local decoded, value = pcall(JSON.decode, body or "")
            if not decoded then reader:close(); return false end
            manifest = value
        else
            local data = reader:extractToMemory(entry.index)
            if not data or not Source.imageExtension(data) then reader:close(); return false end
            count = count + 1
        end
    end
    local err = reader.err
    reader:close()
    return not err and type(manifest) == "table" and manifest.chapter_url == expected_url and count > 0 and manifest.pages == count
end

function Turbo:chapters(slug)
    NetworkMgr:runWhenConnected(function()
        local chapters, err, details = Source.chapters(slug)
        if not chapters then self:message(err); return end
        details.slug = slug
        local items = {{ text = "Baixar todos os capítulos públicos", callback = function()
            UIManager:show(ConfirmBox:new{
                text = "Baixar todos os capítulos públicos? Os CBZ concluídos serão mantidos. Pode demorar e ocupar bastante espaço.",
                ok_callback = function() self:startDownloads(details, chapters) end,
            })
        end }}
        for index = #chapters, 1, -1 do
            local chapter = chapters[index]
            local path = self:chapterPath(details, chapter)
            items[#items + 1] = {
                text = (Source.locked(chapter) and "[bloqueado] " or "") .. "Capítulo " .. tostring(chapter.number) .. " · " .. (chapter.title or ""),
                callback = function()
                    if lfs.attributes(path, "mode") == "file" then self:openBook(path)
                    elseif Source.locked(chapter) then self:message("Capítulo bloqueado no AsuraScans")
                    else self:startDownloads(details, {chapter}, true) end
                end,
            }
        end
        self:showMenu(details.title, items)
    end)
end

function Turbo:openBook(path)
    for menu in pairs(self.menus or {}) do UIManager:close(menu) end
    self.menus = {}
    if self.ui.document then self.ui:switchDocument(path) else ReaderUI:showReader(path) end
end

function Turbo:library(directory)
    directory = directory or self.root
    if lfs.attributes(directory, "mode") ~= "directory" then self:message("Nenhum download ainda"); return end
    local items = {}
    for name in lfs.dir(directory) do
        if name ~= "." and name ~= ".." then
            local path = directory .. "/" .. name
            local mode = lfs.attributes(path, "mode")
            if mode == "directory" then
                items[#items + 1] = { text = name .. "/", callback = function() self:library(path) end }
            elseif mode == "file" and name:match("%.cbz$") then
                items[#items + 1] = { text = name, callback = function() self:openBook(path) end }
            end
        end
    end
    table.sort(items, function(a, b) return natural(a.text, b.text) end)
    if #items == 0 then self:message("Nenhum CBZ nesta pasta"); return end
    self:showMenu("Kindle Turbo · Downloads", items)
end

function Turbo:downloadDirectory()
    local dialog
    dialog = InputDialog:new{
        title = "Pasta dos downloads", input = self.root,
        buttons = {{
            { text = "Cancelar", callback = function() UIManager:close(dialog) end },
            { text = "Salvar", callback = function()
                local path = dialog:getInputText()
                if path:sub(1, 1) ~= "/" then self:message("Use um caminho absoluto"); return end
                path = path:gsub("/+$", "")
                if path == "" then self:message("Escolha uma pasta, não a raiz do sistema"); return end
                self.root = path
                self.settings:saveSetting("download_dir", self.root)
                self.settings:flush()
                UIManager:close(dialog)
            end },
        }},
    }
    UIManager:show(dialog)
    dialog:onShowKeyboard()
end

function Turbo:startDownloads(details, chapters, open_after)
    if self.job then self:message("Já há um download em andamento"); return end
    local ok, archive = pcall(require, "ffi/archiver")
    if not ok or not archive.Writer or not archive.Writer.setZipCompression then
        self:message("Esta versão do KOReader não oferece gravação CBZ via ffi/archiver. Atualize o KOReader ou baixe pelo PC.")
        return
    end
    NetworkMgr:runWhenConnected(function()
        if self.job then self:message("Já há um download em andamento"); return end
        self.job = {
            details = details, chapters = chapters, index = 0, archive = archive,
            open_after = open_after, saved = 0, previous_pause = PluginShare.pause_auto_suspend,
        }
        PluginShare.pause_auto_suspend = true
        UIManager:preventStandby()
        self:nextChapter()
    end)
end

function Turbo:finishDownload(err, silent)
    local job = self.job
    if not job then return end
    if job.writer then job.writer:close() end
    if job.partial then os.remove(job.partial) end
    if job.dialog then UIManager:close(job.dialog) end
    self.job = nil
    PluginShare.pause_auto_suspend = job.previous_pause
    UIManager:allowStandby()
    if silent then return end
    if err then self:message(err .. "\nOs capítulos concluídos foram mantidos; tente novamente para continuar.")
    elseif job.open_after and job.path then self:openBook(job.path)
    else self:message("Download concluído: " .. job.saved .. " capítulo(s). Abra Downloads / ler offline.") end
end

function Turbo:progress(text)
    local job = self.job
    if job.dialog then UIManager:close(job.dialog) end
    job.dialog = ButtonDialog:new{
        title = text,
        buttons = {{{ text = "Cancelar download", callback = function() self:finishDownload("Download cancelado") end }}},
    }
    UIManager:show(job.dialog)
end

function Turbo:nextChapter()
    local job = self.job
    if not job then return end
    job.index = job.index + 1
    local chapter = job.chapters[job.index]
    if not chapter then self:finishDownload(); return end
    if Source.locked(chapter) then self:nextChapter(); return end
    job.path = self:chapterPath(job.details, chapter)
    if lfs.attributes(job.path, "mode") == "file" then
        if not self:archiveValid(job.path, self:chapterURL(job.details, chapter)) then
            self:finishDownload("CBZ existente inválido. Mova-o antes de tentar novamente: " .. job.path); return
        end
        UIManager:scheduleIn(0.1, function() if self.job == job then self:nextChapter() end end)
        return
    end
    self:progress("Preparando capítulo " .. chapter.number)
    UIManager:scheduleIn(1, function()
        if self.job ~= job then return end
        local pages, err = Source.pages(job.details.public_url, chapter.number)
        if not pages then self:finishDownload(err); return end
        job.pages, job.page = pages, 0
        local created, reason = util.makePath(job.path:match("^(.*)/"))
        if not created then self:finishDownload(tostring(reason)); return end
        local partial = job.path .. ".part"
        if lfs.attributes(partial) then self:finishDownload("Download parcial já existe: " .. partial); return end
        job.partial = partial
        job.writer = job.archive.Writer:new()
        if not job.writer:open(job.partial, "zip") or not job.writer:setZipCompression("store") then
            self:finishDownload("Não foi possível criar o CBZ: " .. tostring(job.writer.err)); return
        end
        self:nextPage()
    end)
end

function Turbo:nextPage()
    local job = self.job
    if not job then return end
    job.page = job.page + 1
    local url = job.pages[job.page]
    if not url then
        if not job.writer:addFileFromMemory("kindle-turbo.json", JSON.encode{
            source = "asurascans", chapter_url = self:chapterURL(job.details, job.chapters[job.index]), pages = #job.pages,
        }) then self:finishDownload("Falha ao gravar metadados do capítulo"); return end
        job.writer:close()
        job.writer = nil
        if not self:archiveValid(job.partial, self:chapterURL(job.details, job.chapters[job.index])) then
            self:finishDownload("Falha na verificação do CBZ"); return
        end
        local renamed, reason = os.rename(job.partial, job.path)
        if not renamed then self:finishDownload(tostring(reason)); return end
        job.partial = nil
        job.saved = job.saved + 1
        UIManager:scheduleIn(1, function() if self.job == job then self:nextChapter() end end)
        return
    end
    self:progress("Capítulo " .. job.chapters[job.index].number .. " · página " .. job.page .. "/" .. #job.pages)
    UIManager:scheduleIn(1, function()
        if self.job ~= job then return end
        local body, err = Source.get(url)
        if not body then self:finishDownload(err); return end
        local extension
        extension, err = Source.imageExtension(body)
        if not extension then self:finishDownload(err); return end
        if not job.writer:addFileFromMemory(string.format("%05d.%s", job.page, extension), body) then
            self:finishDownload("Falha ao gravar página: " .. tostring(job.writer.err)); return
        end
        self:nextPage()
    end)
end

function Turbo:onCloseWidget()
    if self.job then self:finishDownload(nil, true) end
end

function Turbo:onSuspend()
    if self.job then self:finishDownload("Download interrompido pela suspensão do aparelho") end
end

return Turbo
