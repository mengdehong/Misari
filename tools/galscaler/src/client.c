#include <windows.h>

// This PE runs inside the game's Proton prefix. Pixels stay in the compositor;
// This file carries only Win32 client rectangles, keyed by Wine's X11 window ID.
// Proton can redirect the console handles before launching a PE.
static char snapshot[32768], previous[32768];
static DWORD length;

static void fail(const char *message)
{
    DWORD written;
    WriteFile(GetStdHandle(STD_ERROR_HANDLE), message, lstrlenA(message), &written, NULL);
    ExitProcess(1);
}

static BOOL CALLBACK window_rect(HWND hwnd, LPARAM helper_pid)
{
    HANDLE xid = GetPropW(hwnd, L"__wine_x11_whole_window");
    RECT rect;
    POINT origin = {0}, end;
    if (!xid || !IsWindowVisible(hwnd) || IsIconic(hwnd) || !GetClientRect(hwnd, &rect))
        return TRUE;
    DWORD pid = 0;
    GetWindowThreadProcessId(hwnd, &pid);
    if (pid == (DWORD)helper_pid) return TRUE;
    end.x = rect.right; end.y = rect.bottom;
    if (!ClientToScreen(hwnd, &origin) || !ClientToScreen(hwnd, &end) ||
        end.x <= origin.x || end.y <= origin.y)
        return TRUE;
    if (length + 128 >= sizeof(snapshot)) fail("GalScaler: too many client windows\n");
    length += wsprintfA(snapshot + length, "%lu %ld %ld %ld %ld ",
                        (DWORD)(ULONG_PTR)xid, origin.x, origin.y,
                        end.x - origin.x, end.y - origin.y);
    return TRUE;
}

void mainCRTStartup(void)
{
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    WCHAR *command = GetCommandLineW();
    if (*command == '"') {
        for (command++; *command && *command != '"'; command++) {}
        if (*command) command++;
    } else {
        while (*command && *command != ' ' && *command != '\t') command++;
    }
    while (*command == ' ' || *command == '\t') command++;
    if (!*command) fail("GalScaler: missing game command\n");

    WCHAR path[32768], temporary[32768];
    DWORD path_length = GetEnvironmentVariableW(L"GALSCALER_CLIENT_RECTS", path, ARRAYSIZE(path));
    if (!path_length || path_length >= ARRAYSIZE(path) - 4)
        fail("GalScaler: missing client rectangle path\n");
    lstrcpyW(temporary, path);
    lstrcatW(temporary, L".tmp");

    HANDLE log = GetStdHandle(STD_ERROR_HANDLE);
    SetHandleInformation(log, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
    STARTUPINFOW startup = { .cb = sizeof(startup), .dwFlags = STARTF_USESTDHANDLES,
        .hStdInput = GetStdHandle(STD_INPUT_HANDLE), .hStdOutput = log, .hStdError = log };
    PROCESS_INFORMATION process;
    HANDLE job = CreateJobObjectW(NULL, NULL);
    if (!job || !CreateProcessW(NULL, command, NULL, NULL, TRUE, CREATE_SUSPENDED,
                                NULL, NULL, &startup, &process))
        fail("GalScaler: cannot start game\n");
    if (!AssignProcessToJobObject(job, process.hProcess)) {
        TerminateProcess(process.hProcess, 1);
        fail("GalScaler: cannot track game processes\n");
    }
    if (ResumeThread(process.hThread) == (DWORD)-1) fail("GalScaler: cannot resume game\n");
    CloseHandle(process.hThread);

    for (;;) {
        lstrcpyA(snapshot, "GALSCALER_CLIENT ");
        length = lstrlenA(snapshot);
        EnumWindows(window_rect, (LPARAM)GetCurrentProcessId());
        snapshot[length++] = '\n'; snapshot[length] = 0;
        if (lstrcmpA(snapshot, previous)) {
            HANDLE file = CreateFileW(temporary, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS,
                                      FILE_ATTRIBUTE_NORMAL, NULL);
            DWORD written;
            if (file == INVALID_HANDLE_VALUE ||
                !WriteFile(file, snapshot, length, &written, NULL) || written != length)
                fail("GalScaler: cannot write client rectangles\n");
            CloseHandle(file);
            if (!MoveFileExW(temporary, path, MOVEFILE_REPLACE_EXISTING))
                fail("GalScaler: cannot update client rectangles\n");
            lstrcpyA(previous, snapshot);
        }
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting;
        if (!QueryInformationJobObject(job, JobObjectBasicAccountingInformation,
                                        &accounting, sizeof(accounting), NULL))
            fail("GalScaler: cannot query game processes\n");
        if (!accounting.ActiveProcesses) break;
        Sleep(50);
    }
    DWORD code = 1;
    GetExitCodeProcess(process.hProcess, &code);
    CloseHandle(process.hProcess);
    CloseHandle(job);
    ExitProcess(code);
}
