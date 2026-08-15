#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>

static const OSType AICoachSignature = 'AICH';
static EventHotKeyRef AICoachHotKey = NULL;
static EventHandlerRef AICoachHandler = NULL;

static OSStatus HandleAICoachHotKey(EventHandlerCallRef nextHandler, EventRef event, void *data) {
    (void)nextHandler;
    (void)data;
    EventHotKeyID identifier = {0};
    GetEventParameter(event, kEventParamDirectObject, typeEventHotKeyID, NULL,
                      sizeof(identifier), NULL, &identifier);
    if (identifier.signature != AICoachSignature || identifier.id != 1) return noErr;

    NSTask *task = [[NSTask alloc] init];
    task.executableURL = [NSURL fileURLWithPath:@"/usr/bin/env"];
    task.arguments = @[@"aicoach", @"toggle"];
    task.standardOutput = [NSFileHandle nullDevice];
    task.standardError = [NSFileHandle nullDevice];
    [task launchAndReturnError:NULL];
    return noErr;
}

@interface AICoachAppDelegate : NSObject <NSApplicationDelegate>
@end

@implementation AICoachAppDelegate
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    EventTypeSpec type = {kEventClassKeyboard, kEventHotKeyPressed};
    OSStatus handlerStatus = InstallEventHandler(GetApplicationEventTarget(),
                                                  HandleAICoachHotKey, 1, &type,
                                                  NULL, &AICoachHandler);
    EventHotKeyID identifier = {AICoachSignature, 1};
    OSStatus hotKeyStatus = RegisterEventHotKey(kVK_Space, optionKey, identifier,
                                                GetApplicationEventTarget(), 0,
                                                &AICoachHotKey);
    if (handlerStatus != noErr || hotKeyStatus != noErr) {
        fputs("aicoach-hotkey: Option+Space registration failed (possibly already in use)\n",
              stderr);
        [NSApp terminate:nil];
    }
}

- (void)applicationWillTerminate:(NSNotification *)notification {
    (void)notification;
    if (AICoachHotKey != NULL) UnregisterEventHotKey(AICoachHotKey);
    if (AICoachHandler != NULL) RemoveEventHandler(AICoachHandler);
}
@end

int main(void) {
    @autoreleasepool {
        NSApplication *application = [NSApplication sharedApplication];
        AICoachAppDelegate *delegate = [[AICoachAppDelegate alloc] init];
        application.delegate = delegate;
        [application setActivationPolicy:NSApplicationActivationPolicyAccessory];
        [application run];
    }
    return 0;
}
