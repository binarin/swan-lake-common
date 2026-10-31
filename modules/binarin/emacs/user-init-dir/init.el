;;; -*- mode: emacs-lisp; lexical-binding: t -*-
(setf custom-file (locate-user-emacs-file "custom.el"))
(when (file-exists-p custom-file) (load custom-file))

(set-language-environment "English")
(set-language-environment-input-method "Russian")

(global-unset-key (kbd "C-z"))
(global-unset-key (kbd "C-x C-z"))

(put 'set-goal-column 'disabled nil)
(put 'narrow-to-region 'disabled nil)

(require 'b-visual)
