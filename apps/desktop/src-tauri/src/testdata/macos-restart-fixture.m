#import <AppKit/AppKit.h>
#include <unistd.h>

// Standalone test application, never an official client or its bundle identity.
int main(void) {
  @autoreleasepool {
    NSApplication *app = [NSApplication sharedApplication];
    [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
    NSDictionary *env = NSProcessInfo.processInfo.environment;
    NSString *home = env[@"CODEX_HOME"];
    if (!home) return 2;
    NSDictionary *state = @{
      @"pid": @(getpid()), @"parent": @(getppid()), @"group": @(getpgrp()),
      @"home": home,
      @"providerPreserved": @([env[@"RESTART_FIXTURE_PROVIDER"] isEqualToString:@"synthetic-value"]),
      @"managerIdentityAbsent": @((BOOL)![env[@"__CFBundleIdentifier"] isEqualToString:@"dev.codexx.fake-manager"]),
      @"managerStateAbsent": @((BOOL)(env[@"CODEXX_FIXTURE_ROOT"] == nil && env[@"TAURI_FIXTURE_STATE"] == nil)),
      @"nodeOptionsAbsent": @((BOOL)(env[@"NODE_OPTIONS"] == nil))
    };
    NSData *data = [NSJSONSerialization dataWithJSONObject:state options:0 error:nil];
    if (![data writeToFile:[home stringByAppendingPathComponent:@"started.json"] atomically:YES]) return 3;
    [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
      if ([[NSFileManager defaultManager] fileExistsAtPath:[home stringByAppendingPathComponent:@"quit"]]) {
        [app terminate:nil];
      }
    }];
    [app run];
  }
  return 0;
}
