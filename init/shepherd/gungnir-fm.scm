;; GNU Shepherd service definition for Gungnir FM
;; Compatible with GNU Guix System
(use-modules (shepherd service))

(define gungnir-fm
  (service
    '(gungnir-fm)
    #:requirement '(user-processes)
    #:start (make-forkexec-constructor
             '("gungnir-fm" "--foreground")
             #:log-file "/var/log/gungnir-fm.log")
    #:stop (make-kill-destructor)
    #:respawn? #t))

(register-services (list gungnir-fm))
(start-service gungnir-fm)
