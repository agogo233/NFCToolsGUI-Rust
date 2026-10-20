/*
 * To change this license header, choose License Headers in Project Properties.
 * To change this template file, choose Tools | Templates
 * and open the template in the editor.
 */

/* 
 * File:   main.cpp
 * Author: vk496
 *
 * Created on 7 de noviembre de 2018, 15:49
 */

#include "cmdhfmfhard.h"

int main(int argc, char* argv[]) {

    if (argc == 2) {
        const char* filePath = argv[1];
        mfnestedhard(filePath);
    } else {
        mfnestedhard("./nonces.bin");
    }

    return 0;
}





