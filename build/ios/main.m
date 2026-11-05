#import <UIKit/UIKit.h>

extern void freemonplay_start(void);

int main(int argc, char *argv[]) {
    (void)argc;
    (void)argv;
    @autoreleasepool {
        freemonplay_start();
    }
    return 0;
}
