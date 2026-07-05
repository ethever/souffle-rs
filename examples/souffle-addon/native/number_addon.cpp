#include <souffle/RamTypes.h>

extern "C" souffle::RamDomain plus_one(souffle::RamDomain value) {
    return value + 1;
}
