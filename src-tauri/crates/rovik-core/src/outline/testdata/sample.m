#import <Foundation/Foundation.h>
@interface Point : NSObject
@property (nonatomic) int x;
- (int)distance:(Point *)other;
+ (instancetype)origin;
@end
@implementation Point
- (int)distance:(Point *)other { return 0; }
+ (instancetype)origin { return nil; }
@end
int helper(void) { return 1; }
